# Contributing to fdintercept

Thank you for your interest in contributing to fdintercept! This document
provides guidelines and instructions for contributing to the project.

## Development setup

1. Install the Rust toolchain using [rustup](https://rustup.rs/).
2. Install Python 3.x for pre-commit hooks.
3. Clone the repository:
   ```bash
   git clone https://github.com/jpmelos/fdintercept
   cd fdintercept
   ```
4. Install pre-commit hooks:
   ```bash
   pip install pre-commit==4.2.0
   pre-commit install
   ```

## Development workflow

1. Fork this repository.
2. Create a new branch for your changes.
3. Make your changes.
4. Ensure all tests pass:
   ```bash
   cargo test
   ```
5. Run pre-commit checks:
   ```bash
   pre-commit run --all-files
   ```
6. Check code coverage (requires Docker):
   ```bash
   ./scripts/coverage.sh
   ```

## Code standards

### Rust

- All code must pass `cargo clippy` with our strict lint settings.
- Format code using `cargo fmt`.
- Follow the existing code style and documentation patterns.
- All APIs (even internal ones) must be documented with doc comments.
- Tests are required for new functionality.

### Documentation

- Keep `README.md` up to date with any user-facing changes.
- Use mdformat for consistent Markdown formatting.
- Include code examples in documentation where appropriate.

### Commits

- Use clear, descriptive commit messages.
- Each commit should represent a single logical change.
- Keep commits focused and atomic.

## Pull request process

1. Ensure your code passes all CI checks.
2. Update documentation if needed.
3. Add tests for new functionality.
4. Update the `README.md` if adding new features.
5. Ensure your PR description clearly explains the changes and motivation.

## Testing

- Write unit tests for new functionality.
- Tests should be clear and descriptive.
- Run tests with `cargo test`.
- Tests must pass on both Linux and MacOS.

## Code coverage

The project uses `cargo-tarpaulin` for code coverage analysis. Run:

```bash
./scripts/coverage.sh
```

We don't require 100% coverage, but we require that paths that could be hit in
legitimate usage should be tested.

## Getting help

If you have questions or need help:

1. Check existing issues.
2. Create a new issue with a clear description.
3. Include relevant code snippets and error messages.

## License

By contributing to fdintercept, you agree that your contributions will be
licensed under the MIT License.
