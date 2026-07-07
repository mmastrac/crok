# crok: A literate CLI testing tool

[![Build](https://img.shields.io/github/actions/workflow/status/mmastrac/crok/build.yml?branch=master)](https://github.com/mmastrac/crok/actions/workflows/build.yml)
[![Book](https://img.shields.io/badge/book-online-blue)](https://mmastrac.github.io/crok/)

crok is a literate CLI testing tool that allows you to write tests for command-line applications
using a simple, literate syntax.

For more information, see the [book](https://mmastrac.github.io/crok/) which
contains a full syntax reference and examples.

## Installation

```shell
cargo install crok
```

## Usage

```shell
crok [options] [test-file] [test-file] ...
```

The test runner will exit with a non-zero exit code if the command does not
match the expected output.

<!-- clihelp:start -->

## Syntax

The test files use a simple syntax:

```shell
#!/usr/bin/env crok --v0

# Comments use shell-style syntax
$ <command> …
%DIRECTIVE(s)
! pattern text and/or %{GROK_NAMED_PATTERN}
pattern block {
  ? more patterns
}
*
! etc…
```

### Command Execution and Directives

| Command               | Description                                   |
| --------------------- | --------------------------------------------- |
| `$ <command> …`       | Execute a shell command and match its output  |
| `%EXIT <n>`           | Expect command to exit with specific code n   |
| `%EXIT fail`          | Expect command to exit with any non-zero code |
| `%EXIT any`           | Accept any exit code (including timeouts)     |
| `%EXIT timeout`       | Expect command to timeout                     |
| `%TIMEOUT <duration>` | Set timeout for a command (e.g., 100ms, 5s)   |
| `%SET <variable>`     | Capture command output into a variable        |
| `%EXPECT <alias> <v>` | Expect a grok capture alias to match a value  |
| `%EXPECT_FAILURE`     | Expect pattern matching to fail               |

### Variables and Quoting

_crok_ uses shell-style variable references and quoting to delimit strings
in commands and control structures.

| Quote Type   | Behavior                                              |
| ------------ | ----------------------------------------------------- |
| `'text'`     | Single quotes - literal value, no expansion           |
| `"text"`     | Double quotes - literal value with variable expansion |
| `\char`      | Backslash escape - preserve literal meaning           |
| `$VAR`       | Basic variable reference                              |
| `${VAR}`     | Explicit variable reference                           |
| `$PWD`       | Special variable for working directory                |
| `$TARGET_OS` | Target OS (`linux`, `macos`, `windows`, etc.)         |

### Control Structures

| Structure                 | Description                                            |
| ------------------------- | ------------------------------------------------------ |
| `# <comment>`             | Ignore this line during test execution                 |
| `include <path>;`         | Include another script                                 |
| `if condition { … }`      | Conditionally execute commands                         |
| `for <var> in <…> { … }`  | Iterate over a list of values                          |
| `background { … }`        | Run commands in background (auto-killed on exit)       |
| `defer { … }`             | Execute cleanup commands after block ends (LIFO order) |
| `retry { … }`             | Retry commands until success or timeout                |
| `exit script;`            | Exit script early with success status                  |
| `set <var> <value>;`      | Set environment variable directly                      |
| `cd <directory>;`         | Change working directory                               |
| `using tempdir;`          | Create and use temporary directory (auto-deleted)      |
| `using new dir <name>;`   | Create new directory for testing (auto-deleted)        |
| `using dir <path>;`       | Use existing directory (not deleted)                   |
| `pattern <NAME> <regex>;` | Define custom grok pattern                             |

### Patterns

| Pattern                              | Description                                                  |
| ------------------------------------ | ------------------------------------------------------------ |
| `! <text>`                           | Auto-escaped pattern (literal text matching + grok patterns) |
| `? <pattern>`                        | Raw pattern (regex-style, requires escaping + grok patterns) |
| `!!!`                                | Multi-line auto-escaped pattern block                        |
| `???`                                | Multi-line raw pattern block                                 |
| `"""`                                | Multi-line literal block (no grok expansion)                 |
| `*`                                  | Any pattern (matches any number of lines lazily)             |
| `%{PATTERN_NAME}`                    | Standard grok pattern                                        |
| `%{PATTERN_NAME=(regex)}`            | Custom grok pattern with regex                               |
| `%{PATTERN_NAME:field_name}`         | Named grok pattern with output field                         |
| `%{PATTERN_NAME:field_name=(regex)}` | Custom named grok pattern                                    |
| `repeat { … }`                       | Match pattern multiple times                                 |
| `choice { … }`                       | Match any one of specified patterns                          |
| `unordered { … }`                    | Match patterns in any order                                  |
| `sequence { … }`                     | Match patterns in strict order                               |
| `optional { … }`                     | Make pattern optional (zero or one match)                    |
| `if <condition> { … }`               | Conditionally require patterns                               |
| `not { … }`                          | Negative lookahead pattern                                   |
| `ignore { … }`                       | Skip/ignore certain output patterns                          |
| `reject { … }`                       | Ensure patterns don't appear in output                       |

### Common Grok Patterns

This is a subset of the grok patterns supported by _crok_. See the full list
of supported patterns at <https://docs.rs/grok/latest/grok/patterns/index.html>,
including the full base patterns in the `grok` module:
<https://docs.rs/grok/latest/grok/patterns/grok/index.html>.

| Pattern         | Description               | Example                |
| --------------- | ------------------------- | ---------------------- |
| `%{DATA}`       | Matches any text (lazy)   | `Hello, %{DATA}`       |
| `%{GREEDYDATA}` | Matches any text (greedy) | `Hello, %{GREEDYDATA}` |
| `%{WORD}`       | Matches word characters   | `[%{WORD}]`            |
| `%{NUMBER}`     | Matches numeric values    | `Count: %{NUMBER}`     |

<!-- clihelp:end -->

## Examples

Match exact output:

```shell
#!/usr/bin/env crok --v0

$ printf "a\nb\nc"
! a
! b
! c
```

Match using a [grok](https://mmastrac.github.io/crok/grok-patterns.html)
pattern:

```shell
$ echo "Hello, anything"
? Hello, %{GREEDYDATA}
```

Expect a non-zero exit code:

```shell
$ cat nonexistent-file
%EXIT fail
*
```
