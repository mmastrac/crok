# Pattern Matching

*crok* provides two main types of pattern matching: auto-escaped patterns (`!`)
and raw patterns (`?`). Each has its own use cases and syntax.

## Auto-escaped Patterns (`!`)

Auto-escaped patterns treat non-grok parts as literal text, making them perfect for exact matches:

```bash session
$ printf "[LOG] Hello, world!\n"
! [LOG] Hello, world!
```

## Raw Patterns (`?`)

Raw patterns treat everything as a pattern, requiring special characters to be
escaped with backslash:

```bash session
$ printf "[LOG] Hello, world!\n"
? \[LOG\] Hello, world!
```

You can use `^` and `$` anchors in raw patterns for exact line matching:

```bash session
$ printf "  X  \n"
? ^  X  $
```

## Grok Patterns

*crok* supports [grok patterns](./grok-patterns.md) for flexible matching:

```bash session
$ echo "Hello, anything"
? Hello, %{GREEDYDATA}
```

Common grok patterns:
- `%{DATA}` - Matches any text
- `%{GREEDYDATA}` - Matches any text greedily

You can also customize grok patterns by providing a name and value:

```bash session
$ printf "[LOG] Hello, world!\n"
? \[%{log=(LOG)}\] %{GREEDYDATA}
```

## Multi-line Matching

### Auto-escaped Multi-line (`!!!`)

```bash session
$ printf "a\nb\nc\n"
!!!
a
b
c
!!!
```

### Raw Multi-line (`???`)

```bash session
$ printf "a\nb\nc\n"
???
a
b
c
???
```

When using multi-line patterns, the indentation of the `!!!` or `???` lines is removed from all lines between them. This makes it easy to maintain proper indentation in your test files while matching unindented output:

```bash session
$ printf "abc\n\ndef\n"
  !!!
  abc

  def
  !!!
```

### Literal Multi-line (`"""`)

Literal multi-line blocks are similar to the other raw multi-line blocks, but
they treat all text as literal text.

```bash session
$ printf "We can match things that look grok-like:\n%%{GROKLIKE}\n"
"""
We can match things that look grok-like:
%{GROKLIKE}
"""
```

## Pattern Structures

### Any Pattern (`*`)

The `*` pattern matches any number of lines lazily, completing when the next structure matches. It can be used at the start, middle, or end of patterns:

```bash session
# Match any output
$ printf "a\nb\nc\n"
*

# Match start, any middle, end
$ printf "a\nb\nc\nd\ne\n"
! a
! b
*
! d
! e

# Match within repeat
$ printf "start\n1\n2\nend\nstart\n1\n2\nend\n"
repeat {
    ! start
    *
    ! end
}
```

### Pattern Blocks

Pattern blocks allow you to combine multiple patterns in different ways:

#### Repeat

Match a pattern multiple times:

```bash session
$ printf "a\nb\nc\n"
repeat {
    choice {
        ! a
        ! b
        ! c
    }
}
```

#### Choice

Match any one of the specified patterns:

```bash session
$ echo "pattern1"
choice {
    ! pattern1
    ! pattern2
    ! pattern3
}
```

#### Unordered

Match patterns in any order:

```bash session
$ printf "b\na\nc\n"
unordered {
    ! a
    ! b
    ! c
}
```

#### Sequence

Match patterns in strict order:

```bash session
$ printf "a\nb\nc\n"
sequence {
    ! a
    ! b
    ! c
}
```

#### Optional

Make a pattern optional:

```bash session
$ echo "optional output"
optional {
    ! optional output
}
```

#### Not

Negative lookahead patterns are supported using `not`. The pattern will fail if
the pattern matches when looking ahead. If the pattern does not match, it will
succeed but consume no lines.

```bash session
$ echo "Hello World"
not {
    ! ERROR
}
! Hello World
```

This can be useful for better targeting of `reject` lines:

```bash session
$ echo "ERROR: We expect this one"
reject {
    # We don't want to reject this expected error, but any others should fail
    sequence {
        not {
            ! ERROR: We expect this one
        }
        ! ERROR: %{GREEDYDATA}
    }
}
# But remember that it doesn't get consumed!
! ERROR: We expect this one
```

#### Ignore

Ignore blocks are supported at the command and global level. Global ignore blocks are applied to all commands in the test, while command-level ignore blocks are applied to the command only.

Skip certain output:

```bash session
$ printf "WARNING: Something happened\nHello World\n"
ignore {
    ? WARNING: %{DATA}
}
! Hello World
```

#### Reject

Reject blocks are supported at the command and global level. Global reject blocks are applied to all commands in the test, while command-level reject blocks are applied to the command only.

Ensure certain patterns don't appear:

```bash session
$ echo "Hello World"
reject {
    ! ERROR
}
! Hello World
```

#### Conditional Patterns

You can use `if` blocks in patterns to conditionally match output:

```bash session
$ echo `uname -s` specific output
if $TARGET_OS == "linux" {
    ! Linux specific output
}
if $TARGET_OS != "linux" {
    ! %{DATA} specific output
}
```

Note that pattern `if` blocks and control `if` blocks have identical syntax, but one contains patterns and the other contains commands.

## Pattern Examples

### Matching Log Lines

```bash session
$ printf "[INFO] User logged in\n[ERROR] Connection failed\n"
repeat {
    ! [%{WORD}] %{GREEDYDATA}
}
```

### Matching Numbers

```bash session
$ echo "Count: 42"
? Count: %{NUMBER}
```

### Matching Dates

```bash session
$ echo "Date: 20-03-2024"
? Date: %{DATE}
``` 
