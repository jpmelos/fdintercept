use anyhow::{Context, Result};
use clap::Parser;
use non_empty_string::NonEmptyString;
use nonempty::NonEmpty;
use serde::Deserialize;
use std::env;
use std::path::PathBuf;

#[derive(Parser)]
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

struct EnvVars {
    conf: Option<PathBuf>,
    recreate_logs: Option<bool>,
    buffer_size: Option<usize>,
    target: Option<String>,
}

#[derive(Deserialize)]
struct Config {
    stdin_log: Option<PathBuf>,
    stdout_log: Option<PathBuf>,
    stderr_log: Option<PathBuf>,
    recreate_logs: Option<bool>,
    buffer_size: Option<usize>,
    target: Option<String>,
}

#[derive(Debug)]
pub struct Target {
    pub executable: NonEmptyString,
    pub args: Vec<String>,
}

#[derive(Debug)]
pub struct ResolvedSettings {
    pub stdin_log: Option<PathBuf>,
    pub stdout_log: Option<PathBuf>,
    pub stderr_log: Option<PathBuf>,
    pub recreate_logs: bool,
    pub buffer_size: usize,
    pub target: Target,
}

pub fn get_settings() -> Result<ResolvedSettings> {
    let cli_args = CliArgs::parse();
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
