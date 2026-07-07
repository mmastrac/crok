# Advanced Features

This chapter covers advanced features and best practices for using *crok* effectively.

## Complex Pattern Matching

### Nested Patterns

Combine different pattern types for complex matching:

```bash session
$ printf "a\nb\nc\nd\n"
sequence {
    ! a
    repeat {
        choice {
            ! b
            ! c
        }
    }
    ! d
}
```

### Conditional Pattern Matching

Use conditions with patterns:

```bash session
if $TARGET_OS == "linux" {
    $ echo Linux specific output
    ? Linux specific %{GREEDYDATA}
}
```

## Process Management

### Background Processes

Run and manage background processes, using `retry` to wait for the process to start:

```bash session
using tempdir;

background {
    $ python3 -m http.server 60800 2> server.log
    %EXIT any
}

$ echo "OK" > health

retry {
    $ curl -s http://localhost:60800/health
    ! OK
}

# Test the server
$ curl -s http://localhost:60800/health
! OK

# Background processes are automatically killed
```

### Process Cleanup

Ensure proper cleanup with defer:

```bash session
defer {
    $ killall background-server
    %EXIT any
    *
}

background {
    $ python3 -m http.server 60800
    %EXIT any
}

$ echo 1
! 1
```

### Timeouts

Set a timeout for a command:

```bash session
$ sleep 60
%EXIT timeout
%TIMEOUT 100ms
```

## Error Handling

### Expected Failures

Test error conditions with `%EXIT`. `%EXIT any` will allow any exit code or signal, while `%EXIT n` will only allow exit code `n`:

```bash session
$ false
%EXIT 1
```

## Expecting Failures

If you want to verify that a pattern does not match, use `%EXPECT_FAILURE`. This can be useful in certain cases, but you should prefer a `reject { }` block if you just want to test that a certain pattern never matches.

```bash session
$ echo "Hello World"
%EXPECT_FAILURE
! Wrong Output
``` 

## Best Practices

### Test Organization

1. Group related tests together and use descriptive comments.
2. Keep tests focused and atomic
3. Use variables for reusable values
4. Use `ignore` for noisy output, prefer a global `ignore` block over many, repeated `ignore { }` blocks.
5. Add `defer` blocks for cleanup immediately after allocation or creation of resources.

### Example of Complex Test

```bash session
# Global ignore/rejects
ignore {
    ! No configuration file found, creating default at %{GREEDYDATA}
}

reject {
    ! Critical error, corrupted configuration file.
}

# Test server startup and basic functionality
using tempdir;

# Start server in background
background {
    $ echo "{\"status\": \"success\"}" > api.json
    $ echo "OK" > health.json
    $ python3 -m http.server 60900 2> server.log
    %EXIT any
}

defer {
    $ rm server.log
}

# Wait for server to start
retry {
    $ curl -s http://localhost:60900/health.json
    ! OK
}

# Test main functionality
$ curl -s http://localhost:60900/api.json
! {"status": "success"}

# Verify logs
$ cat server.log
repeat {
    choice {
        ? %{IPORHOST} %{GREEDYDATA} code %{NUMBER}, %{GREEDYDATA}
        ? %{IPORHOST} %{GREEDYDATA} "GET /%{DATA} %{DATA}" %{NUMBER} -
    }
}

# Cleanup is automatic!
```
