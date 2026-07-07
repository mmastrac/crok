# Getting Started

## Why **crok**?

**crok** makes it easy to write and maintain tests for command-line
applications. Its syntax is designed to be concise, human-readable, and
powerful, allowing you to express complex test scenarios without extra noise.

If you have not read it yet, see [Overview](./overview.md) for how the runner,
`.cli` files, and integrations fit together.

## Features

- Simple and readable test syntax. See [Basic Usage](./basic-usage.md).
- Support for pattern matching using grok patterns. See
  [Grok Patterns](./grok-patterns.md).
- Flexible output matching with multi-line support. See
  [Pattern Matching](./pattern-matching.md).
- Environment variable management. See
  [Environment and Variables](./environment.md).
- Control structures for complex test scenarios. See
  [Control Structures](./control-structures.md).
- Background process management. See
  [Control Structures](./control-structures.md#background-processes).
- Create temporary directories to use during test runs with automatic deletion.
  See [Environment and Variables](./environment.md#using-temporary-directories).
- Cleanup commands to run after the test. See
  [Control Structures](./control-structures.md#deferred-cleanup).
- Retry logic to re-run commands until they succeed or timeout. See
  [Control Structures](./control-structures.md#retry).

