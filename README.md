# fdintercept

A utility program that intercepts and logs stdin, stdout, and stderr for any
target command.

## Features

- Wraps any command and captures all I/O via stdin, stdout, and stderr.
- Logs each stream to separate files.
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

TODO

## Output

TODO

## Example

TODO

## Building from Source

```bash
git clone https://github.com/jpmelos/fdintercept
cd fdintercept
cargo build --release
```

## Roadmap

- [ ] Transparently intercept stdin, stdout, and stderr
- [ ] Supply target command via configuration file
- [ ] Supply target command via environment variable
- [ ] Define log filenames via CLI
- [ ] Define log filenames via configuration file
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
