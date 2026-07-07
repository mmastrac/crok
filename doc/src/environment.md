# Environment and Variables

*crok* provides powerful features for managing environment variables and working directories.

## Setting Variables

### Using `%SET`

`%SET` can be used to capture all or part of the command output into a variable.

Capture the entire command output into a variable:

```bash session
$ printf "value\n"
%SET MY_VAR
*
```

You can also capture one or more grok captures into a variable. The value will
be contructed from the grok captures and existing environment variables:

```bash session
$ echo "Hello, world!"
%SET CAPTURED_GREETING "${greeting} ${word}"
! %{WORD:greeting}, %{WORD:word}!

$ echo "$CAPTURED_GREETING"
! Hello world
```

### Using `set`

Set environment variables directly:

```bash session
set FOO bar;
set PATH "/usr/local/bin:$PATH";
```

## Variable References

### Basic Reference

Use `$VAR` to reference variables:

```bash session
set FOO bar;
$ echo $FOO
! bar
```

### Explicit Reference

Use `${VAR}` when the variable name is followed by text:

```bash session
set FOO bar;
$ echo ${FOO}123
! bar123
```

## Working Directory Management

The working directory is managed through a special variable `PWD`. This can be set directory, or various commands can change it.

### Changing Directory

Change the current working directory. The `PWD` is updated to the new directory for the duration of the test, unless another command changes it:

```bash session
$ mkdir "subdir";
cd "subdir";
```

### Using Temporary Directories

Create and use a temporary directory. The current working directory is automatically set to the temporary directory, and when the block ends, *the temporary directory is automatically deleted*.

```bash session
using tempdir;
```

### Creating New Directories

Create a new directory for testing. The current working directory is automatically set to the directory, *and it is deleted when the block ends*.

```bash session
using new dir "subdir";
```

### Using Existing Directories

Use an existing directory. The current working directory is automatically set to the directory, and it is *not deleted when the block ends*.

```bash session
using tempdir;
$ mkdir -p subdir
using dir "subdir";
```

## Special Variables

### PWD

The `PWD` variable is special and controls the current working directory:

```bash session
$ mktemp -d
%SET TEMP_DIR
*

# Set PWD to change working directory
$ echo $TEMP_DIR
%SET PWD
*
```

## Environment Variable Examples

### Combining Variables

```bash session
set A 1;
set B 2;
set C "$A $B";
$ echo $C
! 1 2
```

### Using Variables in Commands

```bash session
set DIR "subdir";
using new dir "$DIR";
$ echo $PWD
! %{PATH}/subdir
```

### Conditional Environment Setup

```bash session
if $TARGET_OS == "linux" {
    set PATH "/usr/local/bin:$PATH";
}
```
