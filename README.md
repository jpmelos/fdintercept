# fdintercept

A utility program that intercepts and logs stdin, stdout, and stderr for any
target command.

## Features

- Wraps any command and captures all I/O via stdin, stdout, and stderr.
- Logs each stream to separate files.
- Supports target command configuration via the CLI, an environment variable,
  or a configuration file (`~/.fdinterceptrc.toml`).
- Preserves original program exit codes.
- Handles program termination gracefully.

## Installation

Clone this repository and run:

```bash
git clone https://github.com/jpmelos/fdintercept
cd fdintercept
cargo install --path .
```

## Usage

There are two ways to use fdintercept:

1. Direct command line usage:

```bash
fdintercept -- your-command [args...]
```

2. Via environment variable:

```bash
FDINTERCEPT_TARGET="your-command [args...]" fdintercept
```

3. Via configuration file (`~/.fdinterceptrc.toml`):

```toml
target = "your-command [args...]"
```

Then simply run:

```bash
fdintercept
```

The order of precedence in how the target is defined is:

1. Command line arguments
2. Environment variable
3. Configuration file

### Output

The program creates three log files in the current directory:

- `stdin.log`: Contains all input sent to the program.
- `stdout.log`: Contains all standard output from the program.
- `stderr.log`: Contains all error output from the program.

## CLI arguments

fdintercept accepts the following CLI arguments.

- `--stdin-log`: Filename of the log file that will record stdin traffic. if
  relative, this is relative to the current working directory. Default:
  `stdin.log`.
- `--stdout-log`: Filename of the log file that will record stdout traffic. if
  relative, this is relative to the current working directory. Default:
  `stdout.log`.
- `--stderr-log`: Filename of the log file that will record stderr traffic. if
  relative, this is relative to the current working directory. Default:
  `stderr.log`.
- After `--`: the target command to be wrapped by fdintercept.

If at least one of `--stdin-log`, `--stdout-log`, and `--stderr-log` is
specified, only the specified log files will be created. If none are specified,
they will all be created with their default values. (These can be mixed with
the configuration file fields and if any log filenames are specified there, the
defaults won't be created either.)

### Example

```bash
# Log all stdout I/O for a Python script.
fdintercept --stdout-log /tmp/stdout.log -- python script.py arg1 arg2

# Log all stdin, stdout, and stderr I/O for a Python script.
fdintercept -- python script.py arg1 arg2
```

## Configuration

It is also possible to set target and log files via a configuration file called
`~/fdinterceptrc.toml`.

Here are the accepted fields:

- `target`: The target command that needs to be executed.
- `stdin_log`: Filename of the log file that will record stdin traffic. if
  relative, this is relative to the current working directory. Default:
  `stdin.log`.
- `stdout_log`: Filename of the log file that will record stdout traffic. if
  relative, this is relative to the current working directory. Default:
  `stdout.log`.
- `stderr_log`: Filename of the log file that will record stderr traffic. if
  relative, this is relative to the current working directory. Default:
  `stderr.log`.

If at least one of `stdin_log`, `stdout_log`, and `stderr_log` is specified,
only the specified log files will be created. If none are specified, they will
all be created with their default values. (These can be mixed with the CLI
arguments for log filenames and if any log filenames are specified there, the
defaults won't be created either.)

### Example

This will make fdintercept log all stdout I/O for a Python script:

```toml
target = "python script.py arg1 arg2"
stdout_log = "/tmp/stdout.log"
```

## Building from Source

```bash
git clone https://github.com/jpmelos/fdintercept
cd fdintercept
cargo build --release
```

## Roadmap

- [x] Transparently intercept stdin, stdout, and stderr
- [x] Supply target command via configuration file
- [x] Supply target command via environment variable (`$FDINTERCEPT_TARGET`)
- [x] Define log filenames via CLI
- [x] Define log filenames via configuration file
- [ ] Look for configuration in `$XDG_CONFIG_HOME/fdintercept/rc.toml`
- [ ] Look for configuration in a file passed in via the command line
- [ ] Look for configuration in a file passed in via an environment variable
  (`$FDINTERCEPTRC`)
- [ ] Allow intercepting arbitrary file descriptors
- [ ] Allow definition of message schemas, add separators between messages
- [ ] Add timestamps to messages

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
