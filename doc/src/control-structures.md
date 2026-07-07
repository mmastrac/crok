# Control Structures

*crok* provides several control structures to help you write complex test scenarios.

## For Loops

The `for` block allows you to iterate over a list of values:

```bash session
for OS in "linux" "macos" "windows" {
    $ uname -a | grep $OS
    %EXIT any
    optional {
        ! %{GREEDYDATA}
    }
}
```

## Conditional Blocks

You can use `if` blocks to conditionally execute commands:

```bash session
if TARGET_OS == "linux" {
    $ echo Linux specific output
    ! Linux specific output
}
```

This can also be used to exit the script early:

```bash session
if TARGET_OS == "windows" {
    exit script;
}

# ... other commands ...
```

Note that pattern `if` blocks and control `if` blocks have identical syntax, but
one contains patterns and the other contains commands.

## Background processes

Run commands in the background using `background { }`. When the block ends, the
background process is automatically killed. If the test exits early (e.g., due
to a failure), background processes are also killed.

Commands running in a `background` block have no explicit timeout, but you can
set an explicit timeout for each command with `%TIMEOUT` if needed.

```bash session
using tempdir;

background {
    $ python3 -m http.server 60801 2> server.log
    %EXIT any
}

$ echo "OK" > health

retry {
    $ curl -s http://localhost:60801/health
    ! OK
}
```

## Deferred cleanup

Run commands after the block finishes. Multiple `defer` blocks are executed in
reverse order (last in, first out):

```bash session
defer {
    $ echo "Second cleanup"
    ! Second cleanup
}

defer {
    $ echo "First cleanup"
    ! First cleanup
}

$ echo "Running!"
! Running!

$ echo "Done!"
! Done!
```

## Retry

Retry commands until they succeed or timeout:

```bash session
retry {
    $ true
}
``` 

`retry` uses the global timeout for the whole `retry` block, but you can set a
shorter timeout for the command itself with `%TIMEOUT`:

```bash session
retry {
    $ true
    %TIMEOUT 100ms
}
```

## Early exit

You can exit a script early using `exit script;`. This will cause the script to
exit with a success status while skipping the remaining commands. This is useful
for skipping a test if a prerequisite is not met.

```bash session
$ echo "will run"
! will run

if PREREQUISITE != "value" {
    exit script;
}

$ echo "won't run!"
! won't run!
```

## Include

You can include another script into the current script using `include
"path/to/script.cli";`.

```bash session
include "include/included.cli";
```

```bash session
# included.cli

# These patterns and variables are available in the outer script
pattern MY_PATTERN [abcd]+;
set VARIABLE "value";

# This is run at the time the script is included
$ echo "run in included script"
! run in included script
```

The included script is executed in the current script's context, so it can use
the same variables and commands. The included script is treated as if it was a
block in the outer script.


## Variables

Variables in commands and control structures are lazily expanded using
shell-style variable references.

```bash session
set LINUX "linux";
set WINDOWS "windows";

for OS in "$LINUX" "$WINDOWS" {
    $ uname -a | grep $OS
    %EXIT any
    *
}
```

For more details, see [Quoting](./quoting.md).

## Quoting

Strings in commands and control structures are eagerly unescaped. 

```bash session
set VARIABLE "This is a \"quoted\" string with a hex escape\x21";
$ echo $VARIABLE
! This is a "quoted" string with a hex escape!
```

For more details, see [Quoting](./quoting.md).
