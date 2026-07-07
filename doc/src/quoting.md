# Quoting

*crok* uses shell-style quoting and variable expansion for commands,
command-lines and control structures.

## For Command-Lines (`$`)

Command-lines are the text that is passed to the shell. They are used to
construct commands for the shell to execute.

For command-lines specified in the script, the entire line is passed as-is to
the shell (`/bin/sh` by default), and all unescaping is handled by the shell
itself.

The shell will use POSIX-compliant quoting and unescaping rules.

## For Internal Commands and Control Structures (`set`, `if`, `for`, etc.)

Internal commands and control structures are not passed to the shell, and are
processed by *crok* itself.

Unescaping rules for characters (eg: `\n`, `\xXX`) are applied eagerly at
parsing time, while variable references are lazily expanded at runtime.

## Quoting Reference

### Single Quotes (`'`)

Single quotes preserve the literal value of every character within the quotes.
No characters inside single quotes have special meaning, including the dollar
sign used for variable references.

```bash session
# Command-line
$ echo 'Hello $USER'
! Hello $USER

# Internal command
set MESSAGE 'Hello $USER';
$ echo $MESSAGE
! Hello $USER
```

### Double Quotes (`"`)

Double quotes preserve the literal value of most characters, but still
allow for variable expansion (e.g., `$VAR` or `${VAR}`).

```bash session
set USER "username";

# Command-line
$ echo "Hello $USER"
! Hello username

# Internal command
set MESSAGE "Hello $USER";
$ echo $MESSAGE
! Hello username
```

### Backslash Escaping (`\`)

Backslashes can be used to escape the next character, preserving its
literal meaning. This works both inside double quotes and unquoted text.

You can use '\$` to escape dollar signs so they do not participate in variable
expansion in unquoted text or double-quoted strings.

```bash session
$ echo "Hello \"World\""
! Hello "World"

$ echo Hello\ World
! Hello World

$ echo Hello \$WORLD
! Hello $WORLD

set MESSAGE "Hello \"World\"";
$ echo $MESSAGE
! Hello "World"

set MESSAGE "Hello\nWorld";
$ echo "$MESSAGE"
! Hello
! World

set MESSAGE "Hello \$WORLD";
$ echo $MESSAGE
! Hello $WORLD
```

Note that internal commands support additional escaped characters:

| Escape Sequence | Meaning                                       |
| --------------- | --------------------------------------------- |
| `\n`            | Newline                                       |
| `\t`            | Tab                                           |
| `\r`            | Carriage Return                               |
| `\b`            | Backspace                                     |
| `\0`            | Null byte                                     |
| `\a`            | Alarm (BEL)                                   |
| `\e`            | Escape                                        |
| `\f`            | Form feed                                     |
| `\xFF`          | Hexadecimal byte (where `X` is any hex digit) |

## Multi-line Commands

You can split long command-lines across multiple lines using either backslashes
or quotes:

```bash session
$ echo "This is a very long command that \
spans multiple lines"
! This is a very long command that spans multiple lines

$ echo "This is another way to
split a command across lines"
! This is another way to
! split a command across lines 
```

## Variable References

### Basic Reference (`$VAR`)

Use `$VAR` to reference variables:

```bash session
set FOO bar;
$ echo $FOO
! bar
```

### Explicit Reference (`${VAR}`)

Use `${VAR}` when the variable name is followed by text:

```bash session
set FOO bar;
$ echo ${FOO}123
! bar123
```

## Quoting in Control Structures

Quoting is optional, but important in control structures like `for` loops and
conditional blocks:

```bash session
# All three of these are equivalent!
for OS in "linux" "macos" "windows" {
    $ uname -a | grep $OS
    %EXIT any
    *
}

for OS in linux macos windows {
    $ uname -a | grep $OS
    %EXIT any
    *
}

set LINUX "linux";
set WINDOWS "windows";
set MACOS "macos";

for OS in "$LINUX" "$MACOS" "$WINDOWS" {
    $ uname -a | grep $OS
    %EXIT any
    *
}
```
