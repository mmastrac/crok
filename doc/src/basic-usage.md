# Basic Usage

## Running Tests

To run tests using `crok`, just pass the tests files to the `crok` command:

```bash
crok [options] [test-file] [test-file] ...
```

The test runner will exit with a non-zero exit code if any command does not match its expected output.

## Test File Structure

Each test file should start with the shebang:

```bash session
#!/usr/bin/env crok --v0
```

The `--v0` flag indicates that the test file uses version 0 of the syntax. This
ensures backwards compatibility as the syntax evolves in future versions.

## Basic Commands

### Executing Commands

Commands are prefixed with `$`:

```bash session
$ echo "Hello World"
! Hello World
```

You can split long commands across multiple lines using either backslashes or quotes:

```bash session
$ echo This is a very long command that \
       spans multiple lines
! This is a very long command that spans multiple lines

$ echo "This is another way to
split a command across lines"
! This is another way to
! split a command across lines 
```

Special characters may need to be escaped using backslashes. See
[Quoting](./quoting.md) for more details.

### Comments

Comments start with `#` and are ignored during test execution:

```bash session
# This is a comment
$ echo "Hello World"
! Hello World
```

### Basic Output Matching

The simplest way to match output is using the `!` pattern, which treats non-grok parts as literal text:

```bash session
$ echo "Hello World"
! Hello World
```

## Exit Codes

By default, *crok* expects commands to exit with code 0. You can specify a different expected exit code 
using `%EXIT`. `%` directives appear between the command (`$`) and patterns:

```bash session
$ echo 'fail' && exit 1
%EXIT 1
! fail
```

To expect a command to return a failing exit code (ie: non-zero):

```bash session
$ echo 'fail' && exit 1
%EXIT fail
! fail
```

Or to accept any exit code (this will also accept a command that times out):

```bash session
$ echo 'fail' && exit 1
%EXIT any
! fail
```
