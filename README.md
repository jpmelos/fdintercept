# fdintercept

A utility program that intercepts and logs stdin, stdout, and stderr for any
target command.

## Features

- Wraps any command and captures all I/O via stdin, stdout, and stderr.
- No need for manual pipe setup or shell redirection.
- Logs each stream to separate files.
- Cross-platform, supports Linux and MacOS.
- Clean configuration via the CLI, an environment variable, or a configuration
  file, including the target command.
- Configurable buffer size for I/O operations.
- Preserves original program exit codes.
- Handles process and child process termination gracefully.

## Implementation Notes

While Linux-specific optimizations like `splice()` were considered during
development, fdintercept prioritizes cross-platform compatibility to support
software engineers using both Linux and MacOS systems. For most development and
debugging use cases, the performance difference would be negligible, as the
tool is primarily meant for development and testing rather than production
deployments where every microsecond counts.

## Installation

```bash
cargo install fdintercept
```

## Usage

There are three ways to use fdintercept:

1. Direct command line usage:

```bash
fdintercept -- your-command [args...]
```

2. Via environment variable:

```bash
FDINTERCEPT_TARGET="your-command [args...]" fdintercept
```

3. Via configuration file (see [Configuration](#configuration) below for
   details):

```toml
target = "your-command [args...]"
```

Then simply run:

```bash
fdintercept
```

If defined in more than one way, the order of precedence in how the target
command is resolved is:

1. Command line arguments
2. Environment variable
3. Configuration file

### Output

The program creates three log files in the current directory:

- `stdin.log`: Contains all input sent to the program.
- `stdout.log`: Contains all standard output from the program.
- `stderr.log`: Contains all error output from the program.

## Configuration

fdintercept accepts configuration via CLI arguments, environment variables, and
a configuration file. Precedence follows that order.

### CLI arguments

These have the highest priority. If any setting is defined as a CLI argument,
it won't be overridden by environment variables or a configuration file.

fdintercept accepts the following CLI arguments:

- `--conf`: Path to a configuration file. If relative, this is relative to the
  current working directory.
- `--stdin-log`: Filename of the log file that will record stdin traffic. If
  relative, this is relative to the current working directory. Default:
  `stdin.log`.
- `--stdout-log`: Filename of the log file that will record stdout traffic. If
  relative, this is relative to the current working directory. Default:
  `stdout.log`.
- `--stderr-log`: Filename of the log file that will record stderr traffic. If
  relative, this is relative to the current working directory. Default:
  `stderr.log`.
- `--recreate-logs`: Re-create log files instead of appending to them. Default:
  false.
- `--buffer-size`: Size in bytes of the buffer used for I/O operations.
  Default: 8 KiB.
- After `--`: The target command that will be executed.

If at least one of `--stdin-log`, `--stdout-log`, and `--stderr-log` is
specified, only the specified log files will be created. If none are specified,
they will all be created with their default values. (These can be mixed with
the configuration file fields and if any log filenames are specified here or
there, the defaults won't be created either.)

#### Examples

```bash
# Log all stdout I/O for a Python script with a custom buffer size.
fdintercept --stdout-log /tmp/stdout.log --buffer-size 1024 -- python script.py arg1 arg2

# Log all stdin, stdout, and stderr I/O for a Python script.
fdintercept -- python script.py arg1 arg2

# Use a specific configuration file.
fdintercept --conf /path/to/config.toml
```

### Environment variables

These environment variables will be used, if defined:

- `FDINTERCEPTRC`: Path to a configuration file. If relative, this is relative
  to the current working directory.
- `FDINTERCEPT_RECREATE_LOGS`: Re-create log files instead of appending to
  them. Default: false.
- `FDINTERCEPT_BUFFER_SIZE`: Size in bytes of the buffer used for I/O
  operations. Default: 8 KiB.
- `FDINTERCEPT_TARGET`: The target command that will be executed.

### Configuration file

fdintercept will look for the configuration file in these locations, in this
order:

1. Path specified via `--conf` CLI argument
2. Path specified in `$FDINTERCEPTRC` environment variable
3. `~/.fdinterceptrc.toml`
4. `$XDG_CONFIG_HOME/fdintercept/rc.toml`

Here are the accepted fields:

- `stdin_log`: Filename of the log file that will record stdin traffic. If
  relative, this is relative to the current working directory. Default:
  `stdin.log`.
- `stdout_log`: Filename of the log file that will record stdout traffic. If
  relative, this is relative to the current working directory. Default:
  `stdout.log`.
- `stderr_log`: Filename of the log file that will record stderr traffic. If
  relative, this is relative to the current working directory. Default:
  `stderr.log`.
- `recreate_logs`: Re-create log files instead of appending to them. Default:
  false.
- `buffer_size`: Size in bytes of the buffer used for I/O operations. Default:
  8 KiB.
- `target`: The target command that will be executed.

If at least one of `stdin_log`, `stdout_log`, and `stderr_log` is specified,
only the specified log files will be created. If none are specified, they will
all be created with their default values. (These can be mixed with the CLI
arguments and if any log filenames are specified here or there, the defaults
won't be created either.)

#### Example

This will make fdintercept log all stdout I/O for a Python script with a custom
buffer size:

```toml
target = "python script.py arg1 arg2"
stdout_log = "/tmp/stdout.log"
buffer_size = 1024
```

## Building from source

This assumes you have the Rust toolchain installed locally.

```bash
git clone https://github.com/jpmelos/fdintercept
cd fdintercept
cargo build --release
cargo install --path .
```

## Roadmap

- [x] Transparently intercept stdin, stdout, and stderr
- [x] Supply target command via configuration file
- [x] Supply target command via environment variable (`$FDINTERCEPT_TARGET`)
- [x] Define log filenames via CLI
- [x] Define log filenames via configuration file
- [x] Look for configuration in `$XDG_CONFIG_HOME/fdintercept/rc.toml`
- [x] Look for configuration in a file passed in via the command line
- [x] Look for configuration in a file passed in via an environment variable
  (`$FDINTERCEPTRC`)
- [x] Configure buffer size for I/O operations
- [x] Flag to re-create log files instead of appending to them.
- [ ] Allow definition of message schemas, add separators between messages
- [ ] Add timestamps to messages
- [ ] Allow intercepting arbitrary file descriptors

## License

MIT License

Copyright (c) 2025 João Sampaio

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the “Software”), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED “AS IS”, WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
