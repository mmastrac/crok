# Reference

This reference provides an overview of all _crok_ syntax elements organized
by category.

## Command Execution

| Command                     | Description                                   | Location                                                       |
| --------------------------- | --------------------------------------------- | -------------------------------------------------------------- |
| `$ <command> …`             | Execute a shell command and match its output  | [Basic Usage](./basic-usage.md#executing-commands)             |
| `%EXIT <n>`                 | Expect command to exit with specific code n   | [Basic Usage](./basic-usage.md#exit-codes)                     |
| `%EXIT fail`                | Expect command to exit with any non-zero code | [Basic Usage](./basic-usage.md#exit-codes)                     |
| `%EXIT any`                 | Accept any exit code (including timeouts)     | [Basic Usage](./basic-usage.md#exit-codes)                     |
| `%EXIT timeout`             | Expect command to timeout                     | [Advanced Features](./advanced-features.md#timeouts)           |
| `%TIMEOUT <duration>`       | Set timeout for a command (e.g., 100ms, 5s)   | [Advanced Features](./advanced-features.md#timeouts)           |
| `%SET <variable>`           | Capture full command output into a variable   | [Environment](./environment.md#using-set)                      |
| `%SET <variable> <pattern>` | Capture grok captures into a variable         | [Environment](./environment.md#using-set)                      |
| `%EXPECT <alias> <value>`   | Expect a grok capture to match a value        | [Grok Patterns](./grok-patterns.md#expectations-and-aliases)   |
| `%EXPECT_FAILURE`           | Expect pattern matching to fail               | [Advanced Features](./advanced-features.md#expecting-failures) |

## Variables and Quoting

crok uses shell-style variable references and quoting to delimit strings in
commands and control structures.

| Quote Type | Behavior                                              | Location                                   |
| ---------- | ----------------------------------------------------- | ------------------------------------------ |
| `'text'`   | Single quotes - literal value, no expansion           | [Quoting](./quoting.md#single-quotes)      |
| `"text"`   | Double quotes - literal value with variable expansion | [Quoting](./quoting.md#double-quotes)      |
| `\char`    | Backslash escape - preserve literal meaning           | [Quoting](./quoting.md#backslash-escaping) |
| `$VAR`     | Basic variable reference                              | [Quoting](./quoting.md#basic-reference)    |
| `${VAR}`   | Explicit variable reference                           | [Quoting](./quoting.md#explicit-reference) |
| `$PWD`     | Special variable for working directory                | [Environment](./environment.md#pwd)        |

## Special Variables

These variables are automatically set by _crok_ and available in all
scripts:

| Variable         | Description                               | Values                                        |
| ---------------- | ----------------------------------------- | --------------------------------------------- |
| `$PWD`           | Current working directory                 | Path string                                   |
| `$INITIAL_PWD`   | Working directory when the script started | Path string                                   |
| `$TARGET_OS`     | Target operating system                   | `windows`, `linux`, `macos`, `ios`, `android` |
| `$TARGET_FAMILY` | Target OS family                          | `windows`, `unix`, `wasm`                     |
| `$TARGET_ARCH`   | Target CPU architecture                   | `x86`, `x86_64`, `arm`, `aarch64`             |

## Control Structures

| Structure                       | Description                                            | Location                                                           |
| ------------------------------- | ------------------------------------------------------ | ------------------------------------------------------------------ |
| `# <comment>`                   | Ignore this line during test execution                 | [Control Structures](./control-structures.md#comments)             |
| `include "path/to/script.cli";` | Include a script into the current script               | [Control Structures](./control-structures.md#include)              |
| `if condition { … }`            | Conditionally execute commands or patterns             | [Control Structures](./control-structures.md#conditional-blocks)   |
| `for <var> in <…> { … }`        | Iterate over a list of values                          | [Control Structures](./control-structures.md#for-loops)            |
| `background { … }`              | Run commands in background (auto-killed on exit)       | [Control Structures](./control-structures.md#background-processes) |
| `defer { … }`                   | Execute cleanup commands after block ends (LIFO order) | [Control Structures](./control-structures.md#deferred-cleanup)     |
| `retry { … }`                   | Retry commands until success or timeout                | [Control Structures](./control-structures.md#retry)                |
| `exit script;`                  | Exit script early with success status                  | [Control Structures](./control-structures.md#early-exit)           |
| `set <var> <value>;`            | Set environment variable directly                      | [Environment](./environment.md#using-set)                          |
| `cd <directory>;`               | Change working directory                               | [Environment](./environment.md#changing-directory)                 |
| `using tempdir;`                | Create and use temporary directory (auto-deleted)      | [Environment](./environment.md#using-temporary-directories)        |
| `using new dir <name>;`         | Create new directory for testing (auto-deleted)        | [Environment](./environment.md#creating-new-directories)           |
| `using dir <path>;`             | Use existing directory (not deleted)                   | [Environment](./environment.md#using-existing-directories)         |
| `pattern <NAME> <regex>;`       | Define custom grok pattern                             | [Grok Patterns](./grok-patterns.md#examples)                       |

## Patterns

| Pattern                              | Description                                                  | Location                                                          |
| ------------------------------------ | ------------------------------------------------------------ | ----------------------------------------------------------------- |
| `! <text>`                           | Auto-escaped pattern (literal text matching + grok patterns) | [Pattern Matching](./pattern-matching.md#auto-escaped-patterns)   |
| `? <pattern>`                        | Raw pattern (regex-style, requires escaping + grok patterns) | [Pattern Matching](./pattern-matching.md#raw-patterns)            |
| `!!!`                                | Multi-line auto-escaped pattern block                        | [Pattern Matching](./pattern-matching.md#auto-escaped-multi-line) |
| `???`                                | Multi-line raw pattern block                                 | [Pattern Matching](./pattern-matching.md#raw-multi-line)          |
| `"""`                                | Multi-line literal block                                     | [Pattern Matching](./pattern-matching.md#literal-multi-line)      |
| `*`                                  | Any pattern (matches any number of lines lazily)             | [Pattern Matching](./pattern-matching.md#any-pattern)             |
| `%{PATTERN_NAME}`                    | Standard grok pattern                                        | [Grok Patterns](./grok-patterns.md#syntax)                        |
| `%{PATTERN_NAME=(regex)}`            | Custom grok pattern with regex                               | [Grok Patterns](./grok-patterns.md#syntax)                        |
| `%{PATTERN_NAME:field_name}`         | Named grok pattern with output field                         | [Grok Patterns](./grok-patterns.md#syntax)                        |
| `%{PATTERN_NAME:field_name=(regex)}` | Custom named grok pattern                                    | [Grok Patterns](./grok-patterns.md#syntax)                        |
| `repeat { … }`                       | Match pattern multiple times                                 | [Pattern Matching](./pattern-matching.md#repeat)                  |
| `choice { … }`                       | Match any one of specified patterns                          | [Pattern Matching](./pattern-matching.md#choice)                  |
| `unordered { … }`                    | Match patterns in any order                                  | [Pattern Matching](./pattern-matching.md#unordered)               |
| `sequence { … }`                     | Match patterns in strict order                               | [Pattern Matching](./pattern-matching.md#sequence)                |
| `optional { … }`                     | Make pattern optional (zero or one match)                    | [Pattern Matching](./pattern-matching.md#optional)                |
| `if <condition> { … }`               | Conditionally require patterns                               | [Pattern Matching](./pattern-matching.md#conditional-patterns)    |
| `not { … }`                          | Negative lookahead pattern                                   | [Pattern Matching](./pattern-matching.md#not)                     |
| `ignore { … }`                       | Skip/ignore certain output patterns                          | [Pattern Matching](./pattern-matching.md#ignore)                  |
| `reject { … }`                       | Ensure patterns don't appear in output                       | [Pattern Matching](./pattern-matching.md#reject)                  |

## Common Grok Patterns

This is a subset of the grok patterns supported by crok. See the full list of
supported patterns at <https://docs.rs/grok/latest/grok/patterns/index.html>,
including the full base patterns in the `grok` module:
<https://docs.rs/grok/latest/grok/patterns/grok/index.html>.

| Pattern         | Description               | Example                |
| --------------- | ------------------------- | ---------------------- |
| `%{DATA}`       | Matches any text (lazy)   | `Hello, %{DATA}`       |
| `%{GREEDYDATA}` | Matches any text (greedy) | `Hello, %{GREEDYDATA}` |
| `%{WORD}`       | Matches word characters   | `[%{WORD}]`            |
| `%{NUMBER}`     | Matches numeric values    | `Count: %{NUMBER}`     |
