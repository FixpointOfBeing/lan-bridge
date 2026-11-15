use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};
use tokio::{fs, io::AsyncReadExt};

use crate::{create_shared_file, MAX_FILE};

pub const HELP: &str = "\
用法：
  lan-bridge serve         启动服务（前台运行，日志写入文件）
  lan-bridge log           查看当前日志（不持续跟踪）
  lan-bridge share <file>  复制文件到共享目录（不移动原文件）
  lan-bridge --help        显示帮助

默认目录：~/.local/share/lan-bridge
共享文件：shared-files/；日志：server.log
DATA_DIR 可单独覆盖共享目录。无参数时等同于 serve。";

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve,
    Log,
    Share(PathBuf),
    Help,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, &'static str> {
    let args: Vec<_> = args.into_iter().collect();
    match args.as_slice() {
        [] => Ok(Command::Serve),
        [command] if command == "serve" => Ok(Command::Serve),
        [command] if command == "log" => Ok(Command::Log),
        [command] if command == "--help" || command == "-h" || command == "help" => {
            Ok(Command::Help)
        }
        [command, file] if command == "share" => Ok(Command::Share(file.into())),
        [command] if command == "share" => Err("share 需要一个文件路径"),
        _ => Err("未知命令或参数数量不正确"),
    }
}

pub struct Paths {
    pub shared: PathBuf,
    pub log: PathBuf,
}

impl Paths {
    pub fn from_env() -> io::Result<Self> {
        Self::resolve(std::env::var_os("HOME"), std::env::var_os("DATA_DIR"))
    }

    fn resolve(home: Option<OsString>, data_dir: Option<OsString>) -> io::Result<Self> {
        let home = home
            .filter(|value| !value.is_empty())
            .ok_or_else(|| io::Error::other("未设置 HOME，无法确定数据目录"))?;
        let root = PathBuf::from(home).join(".local/share/lan-bridge");
        Ok(Self {
            shared: data_dir
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("shared-files")),
            log: root.join("server.log"),
        })
    }
}

pub fn show_log(path: &Path) -> io::Result<()> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "还没有日志，请先运行 lan-bridge serve",
            ));
        }
        Err(error) => return Err(error),
    };
    io::copy(&mut file, &mut io::stdout().lock())?;
    Ok(())
}

pub async fn share(source: &Path, dir: &Path) -> io::Result<PathBuf> {
    // Reject special files before opening: opening a FIFO could block forever.
    let metadata = fs::metadata(source).await?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "只能分享普通文件，不支持目录",
        ));
    }
    if metadata.len() > MAX_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "单个文件最大 1 GiB",
        ));
    }
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "文件名必须是有效的 UTF-8"))?;
    let mut input = fs::File::open(source).await?;
    fs::create_dir_all(dir).await?;
    let (mut output, _, destination) = create_shared_file(dir, name).await?;
    let result = async {
        let bytes = tokio::io::copy(&mut (&mut input).take(MAX_FILE + 1), &mut output).await?;
        if bytes > MAX_FILE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "单个文件最大 1 GiB",
            ));
        }
        output.sync_all().await
    }
    .await;
    drop(output);
    if let Err(error) = result {
        let _ = fs::remove_file(&destination).await;
        return Err(error);
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_and_rejects_extra_arguments() {
        for (args, expected) in [
            (vec![], Command::Serve),
            (vec!["serve"], Command::Serve),
            (vec!["log"], Command::Log),
            (vec!["share", "a b.txt"], Command::Share("a b.txt".into())),
            (vec!["--help"], Command::Help),
        ] {
            assert_eq!(
                parse(args.into_iter().map(OsString::from)).unwrap(),
                expected
            );
        }
        for args in [
            vec!["share"],
            vec!["other"],
            vec!["log", "extra"],
            vec!["share", "a", "b"],
        ] {
            assert!(parse(args.into_iter().map(OsString::from)).is_err());
        }
    }

    #[test]
    fn resolves_paths_independent_of_working_directory() {
        let paths = Paths::resolve(Some("/home/example".into()), None).unwrap();
        assert_eq!(
            paths.shared,
            PathBuf::from("/home/example/.local/share/lan-bridge/shared-files")
        );
        assert_eq!(
            paths.log,
            PathBuf::from("/home/example/.local/share/lan-bridge/server.log")
        );
        let custom = Paths::resolve(Some("/home/example".into()), Some("/custom".into())).unwrap();
        assert_eq!(custom.shared, PathBuf::from("/custom"));
        assert_eq!(custom.log, paths.log);
        assert!(Paths::resolve(None, None).is_err());
    }

    #[tokio::test]
    async fn shares_by_copying_and_numbers_duplicates() {
        let state = crate::tests::TestState::new().await;
        let source = state.0.dir.join("original.txt");
        let shared = state.0.dir.join("shared");
        fs::write(&source, "contents").await.unwrap();
        let first = share(&source, &shared).await.unwrap();
        let second = share(&source, &shared).await.unwrap();
        assert_eq!(first.file_name().unwrap(), "original.txt");
        assert_eq!(second.file_name().unwrap(), "original (1).txt");
        assert_eq!(fs::read_to_string(&source).await.unwrap(), "contents");
        assert_eq!(fs::read_to_string(first).await.unwrap(), "contents");
        assert_eq!(fs::read_to_string(second).await.unwrap(), "contents");
        assert!(share(&shared, &shared).await.is_err());
        assert!(share(&state.0.dir.join("missing"), &shared).await.is_err());
        let oversized = state.0.dir.join("oversized");
        let file = fs::File::create(&oversized).await.unwrap();
        file.set_len(MAX_FILE + 1).await.unwrap();
        assert!(share(&oversized, &shared).await.is_err());
        assert!(!shared.join("oversized").exists());
    }
}
