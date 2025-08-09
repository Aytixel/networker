use std::{
    collections::HashSet,
    fmt::{self, Display, Formatter},
    fs::{File, create_dir},
    path::{Path, PathBuf},
    sync::Arc,
};

use futures::Stream;
use nix::{
    dir::Dir,
    fcntl::OFlag,
    libc::IN_ISDIR,
    sched::{CloneFlags, setns, unshare},
    sys::stat::{Mode, stat},
};
use notify::{
    Config, EventKind, INotifyWatcher, RecursiveMode, Watcher,
    event::{CreateKind, ModifyKind, RemoveKind, RenameMode},
};
use tokio::{
    sync::{Notify, RwLock},
    task,
};
use tokio_util::sync::ReusableBoxFuture;

const SELF_NETNS_PATH: &str = "/proc/self/ns/net";
const DEAULT_NETNS_PATH: &str = "/proc/1/ns/net";
const NETNS_PATH: &str = "/run/netns";

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Nix(#[from] nix::Error),
    #[error(transparent)]
    Notify(#[from] notify::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Netns {
    #[default]
    Default,
    Named(String),
}

impl Netns {
    pub fn named<T: AsRef<str>>(netns_name: T) -> Self {
        Self::Named(netns_name.as_ref().to_string())
    }

    pub fn list() -> Vec<Netns> {
        let mut netns = vec![Netns::Default];
        let Ok(default_stat) = stat(DEAULT_NETNS_PATH) else {
            return netns;
        };
        let Ok(mut netns_dir) = Dir::open(
            NETNS_PATH,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_DIRECTORY,
            Mode::empty(),
        ) else {
            return netns;
        };

        for entry in netns_dir.iter() {
            let Ok(entry) = entry else {
                continue;
            };
            let file_name = entry.file_name().to_string_lossy().to_string();

            if [".", ".."].contains(&file_name.as_str()) {
                continue;
            }

            let file_path = Path::new(NETNS_PATH).join(&file_name);
            let Ok(netns_stat) = stat(&file_path) else {
                continue;
            };

            if (netns_stat.st_mode & IN_ISDIR == IN_ISDIR)
                || (netns_stat.st_ino == default_stat.st_ino)
            {
                continue;
            }

            netns.push(Netns::Named(file_path.to_string_lossy().to_string()));
        }

        netns
    }

    pub fn path(&self) -> PathBuf {
        match self {
            Netns::Default => Path::new(DEAULT_NETNS_PATH).to_path_buf(),
            Netns::Named(name) => Path::new(NETNS_PATH).join(name),
        }
    }

    pub fn exists(&self) -> bool {
        self.path().exists()
    }

    pub fn enter(&self) -> Result<NetnsHandle, Error> {
        let initial_netns = File::open(SELF_NETNS_PATH)?;
        let target_netns = File::open(self.path())?;

        unshare(CloneFlags::CLONE_NEWNET)?;
        setns(target_netns, CloneFlags::CLONE_NEWNET)?;

        Ok(NetnsHandle(initial_netns))
    }
}

impl Display for Netns {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Netns::Default => f.write_str("default"),
            Netns::Named(name) => f.write_str(name),
        }
    }
}

pub struct NetnsHandle(File);

impl NetnsHandle {
    pub fn close(self) -> Result<(), Error> {
        unshare(CloneFlags::CLONE_NEWNET)?;
        setns(self.0, CloneFlags::CLONE_NEWNET)?;

        Ok(())
    }
}

pub struct NetnsWatcher {
    default_ino: u64,
    list: RwLock<(HashSet<Netns>, usize)>,
    notif: Notify,
}

impl NetnsWatcher {
    pub fn new() -> Result<Arc<Self>, Error> {
        let default_stat = stat(DEAULT_NETNS_PATH)?;
        let netns_watcher = Arc::new(Self {
            default_ino: default_stat.st_ino,
            list: RwLock::new((HashSet::from_iter(Netns::list()), 0)),
            notif: Notify::new(),
        });

        let _handle: task::JoinHandle<Result<(), Error>> = task::spawn({
            let netns_watcher = netns_watcher.clone();

            async move { netns_watcher.run().await }
        });

        tracing::info!("Netns watcher is watching `{NETNS_PATH}`");

        Ok(netns_watcher)
    }

    async fn run(&self) -> Result<(), Error> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut file_watcher = INotifyWatcher::new(
            move |event| {
                tx.send(event).ok();
            },
            Config::default(),
        )?;
        let netns_path = Path::new(NETNS_PATH);

        if !netns_path.is_dir() {
            create_dir(netns_path)?;
        }

        file_watcher.watch(netns_path, RecursiveMode::NonRecursive)?;

        while let Some(event) = rx.recv().await {
            let Ok(event) = event else {
                continue;
            };

            match event.kind {
                EventKind::Create(CreateKind::Any)
                | EventKind::Create(CreateKind::File)
                | EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
                    self.insert(event.paths.iter().map(PathBuf::as_path)).await;
                    self.notif.notify_waiters();
                }
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                    if event.paths.len() == 2 {
                        self.remove([event.paths[1].as_path()]).await;
                        self.insert([event.paths[0].as_path()]).await;
                        self.notif.notify_waiters();
                    }
                }
                EventKind::Remove(RemoveKind::Any)
                | EventKind::Remove(RemoveKind::File)
                | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                    self.remove(event.paths.iter().map(PathBuf::as_path)).await;
                    self.notif.notify_waiters();
                }
                _ => {}
            }
        }

        Ok(())
    }

    pub async fn list(&self) -> (HashSet<Netns>, usize) {
        self.list.read().await.clone()
    }

    pub async fn wait(&self) {
        self.notif.notified().await
    }

    async fn insert<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let mut list = self.list.write().await;
        let mut changed = false;

        for path in paths {
            let Ok(netns_stat) = stat(path) else {
                continue;
            };

            if (netns_stat.st_mode & IN_ISDIR == IN_ISDIR)
                || (netns_stat.st_ino == self.default_ino)
            {
                continue;
            }

            if list
                .0
                .insert(Netns::Named(path.to_string_lossy().to_string()))
            {
                tracing::info!("Netns added `{}`", path.display());
                changed = true;
            }
        }

        if changed {
            list.1 = list.1.wrapping_add(1);
        }
    }

    async fn remove<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let mut list = self.list.write().await;
        let mut changed = false;

        for path in paths {
            if list
                .0
                .remove(&Netns::Named(path.to_string_lossy().to_string()))
            {
                tracing::info!("Netns removed `{}`", path.display());
                changed = true;
            }
        }

        if changed {
            list.1 = list.1.wrapping_add(1);
        }
    }
}

pub struct NetnsWatcherStream {
    poll: ReusableBoxFuture<'static, (HashSet<Netns>, usize, Arc<NetnsWatcher>)>,
}

impl NetnsWatcherStream {
    pub fn new(watcher: Arc<NetnsWatcher>) -> Self {
        Self {
            poll: ReusableBoxFuture::new(Self::poll(None, watcher)),
        }
    }

    async fn poll(
        modification_index: Option<usize>,
        watcher: Arc<NetnsWatcher>,
    ) -> (HashSet<Netns>, usize, Arc<NetnsWatcher>) {
        loop {
            let (list, new_modification_index) = watcher.list().await;

            if modification_index != Some(new_modification_index) {
                return (list, new_modification_index, watcher);
            }

            watcher.wait().await;
        }
    }
}

impl Stream for NetnsWatcherStream {
    type Item = HashSet<Netns>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let self_mut = self.get_mut();
        let (list, modification_index, watcher) = std::task::ready!(self_mut.poll.poll(cx));

        self_mut
            .poll
            .set(Self::poll(Some(modification_index), watcher));

        std::task::Poll::Ready(Some(list))
    }
}
