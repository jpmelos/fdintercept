use anyhow::{Context, Result};
use clap::Parser;
use non_empty_string::NonEmptyString;
use nonempty::NonEmpty;
use serde::Deserialize;
use std::env::{self};
use std::path::PathBuf;

#[derive(Parser, Default)]
#[command(about, version)]
struct CliArgs {
    /// Path to a configuration file. If relative, this is relative to the current working
    /// directory.
    #[arg(long)]
    conf: Option<PathBuf>,

    /// Filename of the log file that will record stdin traffic. If relative, this is relative to
    /// the current working directory. Default: stdin.log.
    #[arg(long)]
    stdin_log: Option<PathBuf>,

    /// Filename of the log file that will record stdout traffic. If relative, this is relative to
    /// the current working directory. Default: stdout.log.
    #[arg(long)]
    stdout_log: Option<PathBuf>,

    /// Filename of the log file that will record stderr traffic. If relative, this is relative to
    /// the current working directory. Default: stderr.log.
    #[arg(long)]
    stderr_log: Option<PathBuf>,

    /// Re-create log files instead of appending to them. Default: false.
    #[arg(long)]
    recreate_logs: bool,

    /// Size in bytes of the buffer used for I/O operations. Default: 8 KiB.
    #[arg(long)]
    buffer_size: Option<usize>,

    /// The target command that will be executed.
    #[arg(last = true)]
    target: Vec<String>,
}

#[derive(Default, Debug)]
struct EnvVars {
    conf: Option<PathBuf>,
    recreate_logs: Option<bool>,
    buffer_size: Option<usize>,
    target: Option<String>,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
struct Config {
    stdin_log: Option<PathBuf>,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    recreate_logs: Option<bool>,
    buffer_size: Option<usize>,
    target: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Target {
    pub executable: NonEmptyString,
    pub args: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct ResolvedSettings {
    pub(crate) stdin_log: Option<PathBuf>,
    pub(crate) stdout_log: Option<PathBuf>,
    pub(crate) stderr_log: Option<PathBuf>,
    pub(crate) recreate_logs: bool,
    pub(crate) buffer_size: usize,
    pub(crate) target: Target,
}

pub(crate) fn get_settings() -> Result<ResolvedSettings> {
    get_settings_with_raw_cli_args(std::env::args())
}

fn get_settings_with_raw_cli_args<A: IntoIterator<Item = String>>(
    raw_cli_args: A,
) -> Result<ResolvedSettings> {
    let cli_args = CliArgs::parse_from(raw_cli_args);
    let env_vars = get_env_vars().context("Error reading environment variables")?;
    let config = get_config(&cli_args, &env_vars).context("Error reading configuration")?;

    let use_defaults = get_use_defaults(&cli_args, &config);

    Ok(ResolvedSettings {
        stdin_log: get_log_name(LogFd::Stdin, &cli_args, &config, use_defaults, "stdin.log"),
        stdout_log: get_log_name(
            LogFd::Stdout,
            &cli_args,
            &config,
            use_defaults,
            "stdout.log",
        ),
        stderr_log: get_log_name(
            LogFd::Stderr,
            &cli_args,
            &config,
            use_defaults,
            "stderr.log",
        ),
        recreate_logs: get_recreate_logs(&cli_args, &env_vars, &config),
        buffer_size: get_buffer_size(&cli_args, &env_vars, &config),
        target: get_target(&cli_args, &env_vars, &config).context("Error getting target")?,
    })
}

fn get_env_vars() -> Result<EnvVars> {
    Ok(EnvVars {
        conf: {
            match env::var("FDINTERCEPTRC") {
                Ok(env_var) => {
                    if env_var.is_empty() {
                        return Err(anyhow::anyhow!("FDINTERCEPTRC is empty"));
                    }
                    Some(PathBuf::from(env_var))
                }
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Error reading FDINTERCEPTRC environment variable: {}",
                        e
                    ));
                }
            }
        },
        recreate_logs: {
            match env::var("FDINTERCEPT_RECREATE_LOGS") {
                Ok(env_var) => match env_var.parse() {
                    Ok(recreate_logs) => Some(recreate_logs),
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "Error parsing FDINTERCEPT_RECREATE_LOGS environment variable: {}",
                            e
                        ));
                    }
                },
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Error reading FDINTERCEPT_RECREATE_LOGS environment variable: {}",
                        e
                    ));
                }
            }
        },
        buffer_size: {
            match env::var("FDINTERCEPT_BUFFER_SIZE") {
                Ok(env_var) => match env_var.parse() {
                    Ok(buffer_size) => Some(buffer_size),
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "Error parsing FDINTERCEPT_BUFFER_SIZE environment variable: {}",
                            e
                        ));
                    }
                },
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Error reading FDINTERCEPT_BUFFER_SIZE environment variable: {}",
                        e
                    ));
                }
            }
        },
        target: {
            match env::var("FDINTERCEPT_TARGET") {
                Ok(env_var) => Some(env_var),
                Err(std::env::VarError::NotPresent) => None,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Error reading FDINTERCEPT_TARGET environment variable: {}",
                        e
                    ));
                }
            }
        },
    })
}

fn get_config(cli_args: &CliArgs, env_vars: &EnvVars) -> Result<Config> {
    if let Some(ref path) = cli_args.conf {
        return std::fs::read_to_string(path)
            .context(format!(
                "Error reading configuration file {}",
                path.display()
            ))
            .and_then(|contents| parse_config_contents(&contents));
    }

    if let Some(ref path) = env_vars.conf {
        return std::fs::read_to_string(path)
            .context(format!(
                "Error reading configuration file {}",
                path.display()
            ))
            .and_then(|contents| parse_config_contents(&contents));
    }

    match env::var("HOME") {
        Ok(home) => {
            let home_path = PathBuf::from(home).join(".fdinterceptrc.toml");
            match std::fs::read_to_string(&home_path) {
                Ok(contents) => {
                    return parse_config_contents(&contents);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => {
                    return Err(e).context(format!(
                        "Error reading configuration file {}",
                        home_path.display()
                    ));
                }
            }
        }
        Err(std::env::VarError::NotPresent) => (),
        Err(e) => {
            eprintln!("Error reading HOME environment variable: {}", e);
        }
    };

    match env::var("XDG_CONFIG_HOME") {
        Ok(xdg_config_home) => {
            let xdg_path = PathBuf::from(xdg_config_home)
                .join("fdintercept")
                .join("rc.toml");
            match std::fs::read_to_string(&xdg_path) {
                Ok(contents) => {
                    return parse_config_contents(&contents);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => {
                    return Err(e).context(format!(
                        "Error reading configuration file {}",
                        xdg_path.display()
                    ));
                }
            }
        }
        Err(std::env::VarError::NotPresent) => (),
        Err(e) => {
            eprintln!("Error reading XDG_CONFIG_HOME environment variable: {}", e);
        }
    };

    parse_config_contents("")
}

#[inline(always)]
fn parse_config_contents(contents: &str) -> Result<Config> {
    toml::from_str(contents).context("Error parsing TOML configuration")
}

fn get_use_defaults(cli_args: &CliArgs, config: &Config) -> bool {
    cli_args.stdin_log.is_none()
        && cli_args.stdout_log.is_none()
        && cli_args.stderr_log.is_none()
        && config.stdin_log.is_none()
        && config.stdout_log.is_none()
        && config.stderr_log.is_none()
}

enum LogFd {
    Stdin,
    Stdout,
    Stderr,
}

fn get_log_name(
    log_fd: LogFd,
    cli_args: &CliArgs,
    config: &Config,
    use_default: bool,
    default_name: &str,
) -> Option<PathBuf> {
    let cli_name = match log_fd {
        LogFd::Stdin => &cli_args.stdin_log,
        LogFd::Stdout => &cli_args.stdout_log,
        LogFd::Stderr => &cli_args.stderr_log,
    };
    let config_name = match log_fd {
        LogFd::Stdin => &config.stdin_log,
        LogFd::Stdout => &config.stdout_log,
        LogFd::Stderr => &config.stderr_log,
    };
    match (cli_name, config_name) {
        (Some(p), _) => Some(p.clone()),
        (None, Some(p)) => Some(p.clone()),
        (None, None) if use_default => Some(PathBuf::from(default_name)),
        _ => None,
    }
}

fn get_recreate_logs(cli_args: &CliArgs, env_vars: &EnvVars, config: &Config) -> bool {
    cli_args.recreate_logs
        || env_vars
            .recreate_logs
            .or(config.recreate_logs)
            .unwrap_or(false)
}

fn get_buffer_size(cli_args: &CliArgs, env_vars: &EnvVars, config: &Config) -> usize {
    cli_args
        .buffer_size
        .or(env_vars.buffer_size)
        .or(config.buffer_size)
        .unwrap_or(8192)
}

fn get_target(cli_args: &CliArgs, env_vars: &EnvVars, config: &Config) -> Result<Target> {
    match get_target_from_cli_arg(&cli_args.target) {
        Ok(target) => return Ok(target),
        Err(CliArgsTargetParseError::NotDefined) => (),
        Err(e) => return Err(e).context("Error getting target from CLI arguments"),
    };

    if let Some(ref target) = env_vars.target {
        match get_target_from_string(target) {
            Ok(target) => return Ok(target),
            Err(e) => {
                return Err(e)
                    .context("Error getting target from FDINTERCEPT_TARGET environment variable");
            }
        }
    }

    if let Some(ref target) = config.target {
        match get_target_from_string(target) {
            Ok(target) => return Ok(target),
            Err(e) => return Err(e).context("Error getting target from configuration file"),
        }
    }

    Err(anyhow::anyhow!(
        "Target not defined in CLI arguments, FDINTERCEPT_TARGET environment variable, or \
         configuration file"
    ))
}

#[derive(Debug)]
enum CliArgsTargetParseError {
    NotDefined,
    EmptyExecutable,
}

impl std::fmt::Display for CliArgsTargetParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::NotDefined => write!(f, "Target is not defined"),
            Self::EmptyExecutable => write!(f, "Target executable cannot be empty"),
        }
    }
}

impl std::error::Error for CliArgsTargetParseError {}

fn get_target_from_cli_arg(cli_arg: &[String]) -> Result<Target, CliArgsTargetParseError> {
    let target_vec = NonEmpty::from_slice(cli_arg).ok_or(CliArgsTargetParseError::NotDefined)?;
    Ok(Target {
        executable: NonEmptyString::new(target_vec.head)
            .map_err(|_| CliArgsTargetParseError::EmptyExecutable)?,
        args: target_vec.tail,
    })
}

#[derive(Debug)]
enum StringTargetParseError {
    Empty,
    FailedToTokenize,
    EmptyExecutable,
}

impl std::fmt::Display for StringTargetParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::FailedToTokenize => write!(f, "Failed to tokenize target"),
            Self::Empty => write!(f, "Target cannot be empty"),
            Self::EmptyExecutable => write!(f, "Target executable cannot be empty"),
        }
    }
}

impl std::error::Error for StringTargetParseError {}

fn get_target_from_string(target: &str) -> Result<Target, StringTargetParseError> {
    if target.is_empty() {
        return Err(StringTargetParseError::Empty);
    }
    let tokenized_target = shlex::split(target).ok_or(StringTargetParseError::FailedToTokenize)?;
    // unwrap: Safe because we already ensure that target is not empty.
    let target_vec = NonEmpty::from_vec(tokenized_target).unwrap();
    Ok(Target {
        executable: NonEmptyString::new(target_vec.head)
            .map_err(|_| StringTargetParseError::EmptyExecutable)?,
        args: target_vec.tail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    mod get_settings_with_raw_cli_args {
        use super::*;

        #[test]
        #[serial]
        fn from_cli_args() {
            let settings = get_settings_with_raw_cli_args(vec![
                "fdintercept".to_string(),
                "--stdin-log".to_string(),
                "custom_stdin.log".to_string(),
                "--stdout-log".to_string(),
                "custom_stdout.log".to_string(),
                "--stderr-log".to_string(),
                "custom_stderr.log".to_string(),
                "--recreate-logs".to_string(),
                "--buffer-size".to_string(),
                "4096".to_string(),
                "--".to_string(),
                "executable".to_string(),
                "arg1".to_string(),
                "arg2".to_string(),
            ])
            .unwrap();

            assert_eq!(settings.stdin_log, Some(PathBuf::from("custom_stdin.log")));
            assert_eq!(
                settings.stdout_log,
                Some(PathBuf::from("custom_stdout.log"))
            );
            assert_eq!(
                settings.stderr_log,
                Some(PathBuf::from("custom_stderr.log"))
            );
            assert!(settings.recreate_logs);
            assert_eq!(settings.buffer_size, 4096);
            assert_eq!(settings.target.executable.as_str(), "executable");
            assert_eq!(settings.target.args, vec!["arg1", "arg2"]);
        }

        #[test]
        #[serial]
        fn from_env_vars() {
            temp_env::with_vars(
                vec![
                    ("FDINTERCEPT_RECREATE_LOGS", Some("true")),
                    ("FDINTERCEPT_BUFFER_SIZE", Some("2048")),
                    ("FDINTERCEPT_TARGET", Some("executable arg1 arg2")),
                ],
                || {
                    let settings =
                        get_settings_with_raw_cli_args(vec!["intercept".to_string()]).unwrap();

                    assert_eq!(settings.stdin_log, Some(PathBuf::from("stdin.log")));
                    assert_eq!(settings.stdout_log, Some(PathBuf::from("stdout.log")));
                    assert_eq!(settings.stderr_log, Some(PathBuf::from("stderr.log")));
                    assert!(settings.recreate_logs);
                    assert_eq!(settings.buffer_size, 2048);
                    assert_eq!(settings.target.executable.as_str(), "executable");
                    assert_eq!(settings.target.args, vec!["arg1", "arg2"]);
                },
            );
        }

        #[test]
        #[serial]
        fn from_config() {
            let tmp_dir = tempfile::TempDir::new().unwrap();
            let config_path = tmp_dir.path().join("config.toml");
            std::fs::write(
                &config_path,
                r#"
                    stdin_log = "config_stdin.log"
                    stdout_log = "config_stdout.log"
                    stderr_log = "config_stderr.log"
                    recreate_logs = true
                    buffer_size = 1024
                    target = "executable arg1 arg2"
                "#,
            )
            .unwrap();

            let settings = get_settings_with_raw_cli_args(vec![
                "fdintercept".to_string(),
                "--conf".to_string(),
                config_path.to_str().unwrap().to_string(),
            ])
            .unwrap();

            assert_eq!(settings.stdin_log, Some(PathBuf::from("config_stdin.log")));
            assert_eq!(
                settings.stdout_log,
                Some(PathBuf::from("config_stdout.log"))
            );
            assert_eq!(
                settings.stderr_log,
                Some(PathBuf::from("config_stderr.log"))
            );
            assert!(settings.recreate_logs);
            assert_eq!(settings.buffer_size, 1024);
            assert_eq!(settings.target.executable.as_str(), "executable");
            assert_eq!(settings.target.args, vec!["arg1", "arg2"]);
        }

        #[test]
        #[serial]
        fn with_no_log_paths() {
            let settings = get_settings_with_raw_cli_args(vec![
                "fdintercept".to_string(),
                "--".to_string(),
                "executable".to_string(),
                "arg1".to_string(),
                "arg2".to_string(),
            ])
            .unwrap();

            assert_eq!(settings.stdin_log, Some(PathBuf::from("stdin.log")));
            assert_eq!(settings.stdout_log, Some(PathBuf::from("stdout.log")));
            assert_eq!(settings.stderr_log, Some(PathBuf::from("stderr.log")));
            assert!(!settings.recreate_logs);
            assert_eq!(settings.buffer_size, 8192);
            assert_eq!(settings.target.executable.as_str(), "executable");
            assert_eq!(settings.target.args, vec!["arg1", "arg2"]);
        }

        #[test]
        #[serial]
        fn with_invalid_env_var() {
            temp_env::with_vars(
                vec![("FDINTERCEPT_BUFFER_SIZE", Some("not_a_number"))],
                || {
                    assert!(
                        get_settings_with_raw_cli_args(vec![
                            "fdintercept".to_string(),
                            "--".to_string(),
                            "executable".to_string(),
                            "arg1".to_string(),
                            "arg2".to_string(),
                        ])
                        .unwrap_err()
                        .to_string()
                        .contains("Error reading environment variables")
                    );
                },
            );
        }

        #[test]
        #[serial]
        fn with_invalid_config() {
            let tmp_dir = tempfile::TempDir::new().unwrap();
            let config_path = tmp_dir.path().join("config.toml");
            std::fs::write(&config_path, "invalid toml").unwrap();

            let args = vec![
                "fdintercept".to_string(),
                "--conf".to_string(),
                config_path.to_str().unwrap().to_string(),
            ];

            assert!(
                get_settings_with_raw_cli_args(args)
                    .unwrap_err()
                    .to_string()
                    .contains("Error reading configuration")
            );
        }

        #[test]
        #[serial]
        fn test_settings_with_missing_target() {
            assert!(
                get_settings_with_raw_cli_args(vec!["fdintercept".to_string()])
                    .unwrap_err()
                    .to_string()
                    .contains("Error getting target")
            );
        }
    }

    mod get_env_vars {
        use super::*;

        #[test]
        #[serial]
        fn empty_environment() {
            temp_env::with_vars(
                vec![
                    ("FDINTERCEPTRC", None::<&str>),
                    ("FDINTERCEPT_RECREATE_LOGS", None::<&str>),
                    ("FDINTERCEPT_BUFFER_SIZE", None::<&str>),
                    ("FDINTERCEPT_TARGET", None::<&str>),
                ],
                || {
                    let env_vars = get_env_vars().unwrap();
                    assert_eq!(env_vars.conf, None);
                    assert_eq!(env_vars.recreate_logs, None);
                    assert_eq!(env_vars.buffer_size, None);
                    assert_eq!(env_vars.target, None);
                },
            );
        }

        #[test]
        #[serial]
        fn valid_conf() {
            temp_env::with_vars(vec![("FDINTERCEPTRC", Some("/path/to/config"))], || {
                assert_eq!(
                    get_env_vars().unwrap().conf,
                    Some(PathBuf::from("/path/to/config"))
                );
            });
        }

        #[test]
        #[serial]
        fn empty_conf() {
            temp_env::with_vars(vec![("FDINTERCEPTRC", Some(""))], || {
                assert_eq!(
                    get_env_vars().unwrap_err().to_string(),
                    "FDINTERCEPTRC is empty"
                );
            });
        }

        #[test]
        #[serial]
        fn valid_recreate_logs() {
            temp_env::with_vars(vec![("FDINTERCEPT_RECREATE_LOGS", Some("true"))], || {
                assert_eq!(get_env_vars().unwrap().recreate_logs, Some(true));
            });
        }

        #[test]
        #[serial]
        fn invalid_recreate_logs() {
            temp_env::with_vars(
                vec![("FDINTERCEPT_RECREATE_LOGS", Some("not_a_bool"))],
                || {
                    assert!(
                        get_env_vars().unwrap_err().to_string().contains(
                            "Error parsing FDINTERCEPT_RECREATE_LOGS environment variable"
                        )
                    );
                },
            );
        }

        #[test]
        #[serial]
        fn valid_buffer_size() {
            temp_env::with_vars(vec![("FDINTERCEPT_BUFFER_SIZE", Some("1024"))], || {
                assert_eq!(get_env_vars().unwrap().buffer_size, Some(1024));
            });
        }

        #[test]
        #[serial]
        fn invalid_buffer_size() {
            temp_env::with_vars(
                vec![("FDINTERCEPT_BUFFER_SIZE", Some("not_a_number"))],
                || {
                    assert!(
                        get_env_vars()
                            .unwrap_err()
                            .to_string()
                            .contains("Error parsing FDINTERCEPT_BUFFER_SIZE environment variable")
                    );
                },
            );
        }

        #[test]
        #[serial]
        fn valid_target() {
            temp_env::with_vars(vec![("FDINTERCEPT_TARGET", Some("echo hello"))], || {
                assert_eq!(
                    get_env_vars().unwrap().target,
                    Some("echo hello".to_string())
                );
            });
        }

        #[test]
        #[serial]
        fn all_valid_vars() {
            temp_env::with_vars(
                vec![
                    ("FDINTERCEPTRC", Some("/path/to/config")),
                    ("FDINTERCEPT_RECREATE_LOGS", Some("true")),
                    ("FDINTERCEPT_BUFFER_SIZE", Some("1024")),
                    ("FDINTERCEPT_TARGET", Some("echo hello")),
                ],
                || {
                    let env_vars = get_env_vars().unwrap();
                    assert_eq!(env_vars.conf, Some(PathBuf::from("/path/to/config")));
                    assert_eq!(env_vars.recreate_logs, Some(true));
                    assert_eq!(env_vars.buffer_size, Some(1024));
                    assert_eq!(env_vars.target, Some("echo hello".to_string()));
                },
            );
        }
    }

    mod get_config {
        use super::*;
        use std::fs;
        use tempfile::TempDir;

        #[test]
        fn from_cli_args() {
            let tmp_dir = TempDir::new().unwrap();
            let config_path = tmp_dir.path().join("config.toml");
            fs::write(&config_path, "buffer_size = 1024").unwrap();

            let cli_args = CliArgs {
                conf: Some(config_path),
                ..Default::default()
            };
            let env_vars = EnvVars::default();

            assert_eq!(
                get_config(&cli_args, &env_vars).unwrap().buffer_size,
                Some(1024)
            );
        }

        #[test]
        fn from_cli_args_nonexistent_file() {
            let cli_args = CliArgs {
                conf: Some(PathBuf::from("/nonexistent/config.toml")),
                ..Default::default()
            };
            let env_vars = EnvVars::default();

            assert!(
                get_config(&cli_args, &env_vars)
                    .unwrap_err()
                    .to_string()
                    .contains("Error reading configuration file")
            );
        }

        #[test]
        fn from_cli_args_invalid_toml() {
            let tmp_dir = TempDir::new().unwrap();
            let config_path = tmp_dir.path().join("config.toml");
            fs::write(&config_path, "invalid toml").unwrap();

            let cli_args = CliArgs {
                conf: Some(config_path),
                ..Default::default()
            };
            let env_vars = EnvVars::default();

            assert!(
                get_config(&cli_args, &env_vars)
                    .unwrap_err()
                    .to_string()
                    .contains("Error parsing TOML configuration")
            );
        }

        #[test]
        fn from_env_vars() {
            let tmp_dir = TempDir::new().unwrap();
            let config_path = tmp_dir.path().join("config.toml");
            fs::write(&config_path, "buffer_size = 2048").unwrap();

            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                conf: Some(config_path),
                ..Default::default()
            };

            assert_eq!(
                get_config(&cli_args, &env_vars).unwrap().buffer_size,
                Some(2048)
            );
        }

        #[test]
        fn from_env_vars_nonexistent_file() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                conf: Some(PathBuf::from("/nonexistent/config.toml")),
                ..Default::default()
            };

            assert!(
                get_config(&cli_args, &env_vars)
                    .unwrap_err()
                    .to_string()
                    .contains("Error reading configuration file")
            );
        }

        #[test]
        fn from_env_vars_invalid_toml() {
            let tmp_dir = TempDir::new().unwrap();
            let config_path = tmp_dir.path().join("config.toml");
            fs::write(&config_path, "invalid toml").unwrap();

            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                conf: Some(config_path),
                ..Default::default()
            };

            assert!(
                get_config(&cli_args, &env_vars)
                    .unwrap_err()
                    .to_string()
                    .contains("Error parsing TOML configuration")
            );
        }

        #[test]
        #[serial]
        fn from_home_dir() {
            let tmp_dir = TempDir::new().unwrap();
            let config_path = tmp_dir.path().join(".fdinterceptrc.toml");
            fs::write(&config_path, "buffer_size = 4096").unwrap();

            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();

            temp_env::with_vars(
                vec![("HOME", Some(tmp_dir.path().to_str().unwrap()))],
                || {
                    assert_eq!(
                        get_config(&cli_args, &env_vars).unwrap().buffer_size,
                        Some(4096)
                    );
                },
            );
        }

        #[test]
        #[serial]
        fn from_home_dir_invalid_toml() {
            let tmp_dir = TempDir::new().unwrap();
            let config_path = tmp_dir.path().join(".fdinterceptrc.toml");
            fs::write(&config_path, "invalid toml").unwrap();

            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();

            temp_env::with_vars(
                vec![("HOME", Some(tmp_dir.path().to_str().unwrap()))],
                || {
                    assert!(
                        get_config(&cli_args, &env_vars)
                            .unwrap_err()
                            .to_string()
                            .contains("Error parsing TOML configuration")
                    );
                },
            );
        }

        #[test]
        #[serial]
        fn if_home_dir_not_found_move_on() {
            let tmp_dir = TempDir::new().unwrap();

            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();

            temp_env::with_vars(
                vec![("HOME", Some(tmp_dir.path().to_str().unwrap()))],
                || {
                    assert_eq!(get_config(&cli_args, &env_vars).unwrap(), Config::default());
                },
            );
        }

        #[test]
        #[serial]
        fn from_xdg_config_home() {
            let tmp_dir = TempDir::new().unwrap();
            fs::create_dir_all(tmp_dir.path().join("fdintercept")).unwrap();
            let config_path = tmp_dir.path().join("fdintercept/rc.toml");
            fs::write(&config_path, "buffer_size = 8192").unwrap();

            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();

            temp_env::with_vars(
                vec![
                    ("HOME", None),
                    ("XDG_CONFIG_HOME", Some(tmp_dir.path().to_str().unwrap())),
                ],
                || {
                    assert_eq!(
                        get_config(&cli_args, &env_vars).unwrap().buffer_size,
                        Some(8192)
                    );
                },
            );
        }

        #[test]
        #[serial]
        fn from_xdg_config_home_invalid_toml() {
            let tmp_dir = TempDir::new().unwrap();
            fs::create_dir_all(tmp_dir.path().join("fdintercept")).unwrap();
            let config_path = tmp_dir.path().join("fdintercept/rc.toml");
            fs::write(&config_path, "invalid toml").unwrap();

            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();

            temp_env::with_vars(
                vec![
                    ("HOME", None),
                    ("XDG_CONFIG_HOME", Some(tmp_dir.path().to_str().unwrap())),
                ],
                || {
                    assert!(
                        get_config(&cli_args, &env_vars)
                            .unwrap_err()
                            .to_string()
                            .contains("Error parsing TOML configuration")
                    );
                },
            );
        }

        #[test]
        #[serial]
        fn if_xdg_config_home_dir_not_found_move_on() {
            let tmp_dir = TempDir::new().unwrap();

            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();

            temp_env::with_vars(
                vec![
                    ("HOME", None),
                    ("XDG_CONFIG_HOME", Some(tmp_dir.path().to_str().unwrap())),
                ],
                || {
                    assert_eq!(get_config(&cli_args, &env_vars).unwrap(), Config::default());
                },
            );
        }

        #[test]
        #[serial]
        fn no_config_found() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();

            temp_env::with_vars(
                vec![("HOME", None::<&str>), ("XDG_CONFIG_HOME", None::<&str>)],
                || {
                    assert_eq!(get_config(&cli_args, &env_vars).unwrap(), Config::default());
                },
            );
        }
    }

    mod get_use_defaults {
        use super::*;

        #[test]
        fn no_logs() {
            let cli_args = CliArgs::default();
            let config = Config::default();

            assert!(get_use_defaults(&cli_args, &config));
        }

        #[test]
        fn cli_stdin_log() {
            let cli_args = CliArgs {
                stdin_log: Some(PathBuf::from("stdin.log")),
                ..Default::default()
            };
            let config = Config::default();

            assert!(!get_use_defaults(&cli_args, &config));
        }

        #[test]
        fn cli_stdout_log() {
            let cli_args = CliArgs {
                stdout_log: Some(PathBuf::from("stdout.log")),
                ..Default::default()
            };
            let config = Config::default();

            assert!(!get_use_defaults(&cli_args, &config));
        }

        #[test]
        fn cli_stderr_log() {
            let cli_args = CliArgs {
                stderr_log: Some(PathBuf::from("stderr.log")),
                ..Default::default()
            };
            let config = Config::default();

            assert!(!get_use_defaults(&cli_args, &config));
        }

        #[test]
        fn config_stdin_log() {
            let cli_args = CliArgs::default();
            let config = Config {
                stdin_log: Some(PathBuf::from("stdin.log")),
                ..Default::default()
            };

            assert!(!get_use_defaults(&cli_args, &config));
        }

        #[test]
        fn config_stdout_log() {
            let cli_args = CliArgs::default();
            let config = Config {
                stdout_log: Some(PathBuf::from("stdout.log")),
                ..Default::default()
            };

            assert!(!get_use_defaults(&cli_args, &config));
        }

        #[test]
        fn config_stderr_log() {
            let cli_args = CliArgs::default();
            let config = Config {
                stderr_log: Some(PathBuf::from("stderr.log")),
                ..Default::default()
            };

            assert!(!get_use_defaults(&cli_args, &config));
        }
    }

    mod get_log_name {
        use super::*;

        #[test]
        fn from_cli_args() {
            let cli_args = CliArgs {
                stdin_log: Some(PathBuf::from("cli.log")),
                ..Default::default()
            };
            let config = Config::default();

            assert_eq!(
                get_log_name(LogFd::Stdin, &cli_args, &config, true, "default.log"),
                Some(PathBuf::from("cli.log"))
            );
        }

        #[test]
        fn from_config() {
            let cli_args = CliArgs::default();
            let config = Config {
                stdin_log: Some(PathBuf::from("config.log")),
                ..Default::default()
            };

            assert_eq!(
                get_log_name(LogFd::Stdin, &cli_args, &config, true, "default.log"),
                Some(PathBuf::from("config.log"))
            );
        }

        #[test]
        fn from_default() {
            let cli_args = CliArgs::default();
            let config = Config::default();

            assert_eq!(
                get_log_name(LogFd::Stdin, &cli_args, &config, true, "default.log"),
                Some(PathBuf::from("default.log"))
            );
        }

        #[test]
        fn no_default_returns_none() {
            let cli_args = CliArgs::default();
            let config = Config::default();

            assert_eq!(
                get_log_name(LogFd::Stdin, &cli_args, &config, false, "default.log"),
                None
            );
        }

        #[test]
        fn cli_args_take_precedence_over_config() {
            let cli_args = CliArgs {
                stdin_log: Some(PathBuf::from("cli.log")),
                ..Default::default()
            };
            let config = Config {
                stdout_log: Some(PathBuf::from("config.log")),
                ..Default::default()
            };

            assert_eq!(
                get_log_name(LogFd::Stdin, &cli_args, &config, true, "default.log"),
                Some(PathBuf::from("cli.log"))
            );
        }

        #[test]
        fn test_all_log_fd_variants() {
            let cli_args = CliArgs {
                stdin_log: Some(PathBuf::from("stdin.log")),
                stdout_log: Some(PathBuf::from("stdout.log")),
                stderr_log: Some(PathBuf::from("stderr.log")),
                ..Default::default()
            };
            let config = Config::default();

            assert_eq!(
                get_log_name(LogFd::Stdin, &cli_args, &config, true, "default.log"),
                Some(PathBuf::from("stdin.log"))
            );
            assert_eq!(
                get_log_name(LogFd::Stdout, &cli_args, &config, true, "default.log"),
                Some(PathBuf::from("stdout.log"))
            );
            assert_eq!(
                get_log_name(LogFd::Stderr, &cli_args, &config, true, "default.log"),
                Some(PathBuf::from("stderr.log"))
            );
        }
    }

    mod get_recreate_logs {
        use super::*;

        #[test]
        fn cli_args_true() {
            let cli_args = CliArgs {
                recreate_logs: true,
                ..Default::default()
            };
            let env_vars = EnvVars::default();
            let config = Config::default();

            assert!(get_recreate_logs(&cli_args, &env_vars, &config));
        }

        #[test]
        fn from_env_vars_true() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                recreate_logs: Some(true),
                ..Default::default()
            };
            let config = Config::default();

            assert!(get_recreate_logs(&cli_args, &env_vars, &config));
        }

        #[test]
        fn from_config_true() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();
            let config = Config {
                recreate_logs: Some(true),
                ..Default::default()
            };

            assert!(get_recreate_logs(&cli_args, &env_vars, &config));
        }

        #[test]
        fn default_false() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();
            let config = Config::default();

            assert!(!get_recreate_logs(&cli_args, &env_vars, &config));
        }

        #[test]
        fn precedence_cli_args_over_env_vars() {
            let cli_args = CliArgs {
                recreate_logs: true,
                ..Default::default()
            };
            let env_vars = EnvVars {
                recreate_logs: Some(false),
                ..Default::default()
            };
            let config = Config::default();

            assert!(get_recreate_logs(&cli_args, &env_vars, &config));
        }

        #[test]
        fn precedence_env_vars_over_config() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                recreate_logs: Some(true),
                ..Default::default()
            };
            let config = Config {
                recreate_logs: Some(false),
                ..Default::default()
            };

            assert!(get_recreate_logs(&cli_args, &env_vars, &config));
        }

        #[test]
        fn precedence_cli_args_over_config() {
            let cli_args = CliArgs {
                recreate_logs: true,
                ..Default::default()
            };
            let env_vars = EnvVars::default();
            let config = Config {
                recreate_logs: Some(false),
                ..Default::default()
            };

            assert!(get_recreate_logs(&cli_args, &env_vars, &config));
        }
    }

    mod get_buffer_size {
        use super::*;

        #[test]
        fn cli_args() {
            let cli_args = CliArgs {
                buffer_size: Some(4096),
                ..Default::default()
            };
            let env_vars = EnvVars::default();
            let config = Config::default();

            assert_eq!(get_buffer_size(&cli_args, &env_vars, &config), 4096);
        }

        #[test]
        fn from_env_vars() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                buffer_size: Some(2048),
                ..Default::default()
            };
            let config = Config::default();

            assert_eq!(get_buffer_size(&cli_args, &env_vars, &config), 2048);
        }

        #[test]
        fn from_config() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();
            let config = Config {
                buffer_size: Some(1024),
                ..Default::default()
            };

            assert_eq!(get_buffer_size(&cli_args, &env_vars, &config), 1024);
        }

        #[test]
        fn default() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();
            let config = Config::default();

            assert_eq!(get_buffer_size(&cli_args, &env_vars, &config), 8192);
        }

        #[test]
        fn precedence_cli_args_over_env_vars() {
            let cli_args = CliArgs {
                buffer_size: Some(4096),
                ..Default::default()
            };
            let env_vars = EnvVars {
                buffer_size: Some(2048),
                ..Default::default()
            };
            let config = Config::default();

            assert_eq!(get_buffer_size(&cli_args, &env_vars, &config), 4096);
        }

        #[test]
        fn precedence_env_vars_over_config() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                buffer_size: Some(2048),
                ..Default::default()
            };
            let config = Config {
                buffer_size: Some(1024),
                ..Default::default()
            };

            assert_eq!(get_buffer_size(&cli_args, &env_vars, &config), 2048);
        }

        #[test]
        fn precedence_cli_args_over_config() {
            let cli_args = CliArgs {
                buffer_size: Some(4096),
                ..Default::default()
            };
            let env_vars = EnvVars::default();
            let config = Config {
                buffer_size: Some(1024),
                ..Default::default()
            };

            assert_eq!(get_buffer_size(&cli_args, &env_vars, &config), 4096);
        }
    }

    mod get_target {
        use super::*;

        #[test]
        fn from_cli_args_success() {
            let cli_args = CliArgs {
                target: vec![
                    "executable".to_string(),
                    "arg1".to_string(),
                    "arg2".to_string(),
                ],
                ..Default::default()
            };
            let env_vars = EnvVars::default();
            let config = Config::default();

            let target = get_target(&cli_args, &env_vars, &config).unwrap();
            assert_eq!(target.executable.as_str(), "executable");
            assert_eq!(target.args, vec!["arg1", "arg2"]);
        }

        #[test]
        fn from_cli_args_invalid() {
            let cli_args = CliArgs {
                target: vec!["".to_string(), "arg1".to_string()],
                ..Default::default()
            };
            let env_vars = EnvVars::default();
            let config = Config::default();

            assert!(
                get_target(&cli_args, &env_vars, &config)
                    .unwrap_err()
                    .to_string()
                    .contains("Error getting target from CLI arguments")
            );
        }

        #[test]
        fn from_env_vars_success() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                target: Some("executable arg1 arg2".to_string()),
                ..Default::default()
            };
            let config = Config::default();

            let target = get_target(&cli_args, &env_vars, &config).unwrap();
            assert_eq!(target.executable.as_str(), "executable");
            assert_eq!(target.args, vec!["arg1", "arg2"]);
        }

        #[test]
        fn from_env_vars_invalid() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars {
                target: Some("executable \"unclosed quote arg1 arg2".to_string()),
                ..Default::default()
            };
            let config = Config::default();

            assert!(
                get_target(&cli_args, &env_vars, &config)
                    .unwrap_err()
                    .to_string()
                    .contains("Error getting target from FDINTERCEPT_TARGET environment variable")
            );
        }

        #[test]
        fn from_config_success() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();
            let config = Config {
                target: Some("executable arg1 arg2".to_string()),
                ..Default::default()
            };

            let target = get_target(&cli_args, &env_vars, &config).unwrap();
            assert_eq!(target.executable.as_str(), "executable");
            assert_eq!(target.args, vec!["arg1", "arg2"]);
        }

        #[test]
        fn from_config_invalid() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();
            let config = Config {
                target: Some("\"\" arg1 arg2".to_string()),
                ..Default::default()
            };

            assert!(
                get_target(&cli_args, &env_vars, &config)
                    .unwrap_err()
                    .to_string()
                    .contains("Error getting target from configuration file")
            );
        }

        #[test]
        fn not_defined() {
            let cli_args = CliArgs::default();
            let env_vars = EnvVars::default();
            let config = Config::default();

            assert!(
            get_target(&cli_args, &env_vars, &config)
                .unwrap_err()
                .to_string()
                .contains(
                    "Target not defined in CLI arguments, FDINTERCEPT_TARGET environment variable, \
                     or configuration file"
                )
        );
        }
    }

    mod get_target_from_cli_args {
        use super::*;

        #[test]
        fn valid() {
            let args = vec![
                "executable".to_string(),
                "arg1".to_string(),
                "arg2".to_string(),
            ];
            let target = get_target_from_cli_arg(&args).unwrap();
            assert_eq!(target.executable.as_str(), "executable");
            assert_eq!(target.args, vec!["arg1", "arg2"]);
        }

        #[test]
        fn empty() {
            let args = vec![];
            assert!(matches!(
                get_target_from_cli_arg(&args),
                Err(CliArgsTargetParseError::NotDefined)
            ));
        }

        #[test]
        fn with_empty_executable() {
            let args = vec!["".to_string(), "arg1".to_string(), "arg2".to_string()];
            assert!(matches!(
                get_target_from_cli_arg(&args),
                Err(CliArgsTargetParseError::EmptyExecutable)
            ));
        }
    }

    mod get_target_from_string {
        use super::*;

        #[test]
        fn valid() {
            let target = get_target_from_string("executable arg1 arg2").unwrap();
            assert_eq!(target.executable.as_str(), "executable");
            assert_eq!(target.args, vec!["arg1", "arg2"]);
        }

        #[test]
        fn empty() {
            assert!(matches!(
                get_target_from_string(""),
                Err(StringTargetParseError::Empty)
            ));
        }

        #[test]
        fn with_quoted_args() {
            let target = get_target_from_string("executable \"arg with spaces\" arg2").unwrap();
            assert_eq!(target.executable.as_str(), "executable");
            assert_eq!(target.args, vec!["arg with spaces", "arg2"]);
        }

        #[test]
        fn with_wrongly_quoted_args() {
            assert!(matches!(
                get_target_from_string("executable \"unclosed quote arg1 arg2"),
                Err(StringTargetParseError::FailedToTokenize)
            ));
        }

        #[test]
        fn with_empty_executable() {
            assert!(matches!(
                get_target_from_string("\"\" arg1 arg2"),
                Err(StringTargetParseError::EmptyExecutable)
            ));
        }
    }
}
