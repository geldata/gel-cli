test: cargo-test clitest

cargo-test:
  cargo test

cargo-build:
  cargo build

# just clitest runs all tests, in quiet mode.
# just clitest {test1} {test2} runs only the specified tests, in verbose mode.
clitest *ARGS: cargo-build
  #!/bin/bash
  set -euf -o pipefail
  CLITEST_VERSION=$(grep 'CLITEST_VERSION:' .github/workflows/new-tests.yml | sed 's/.*CLITEST_VERSION: "=\([^"]*\)"/\1/')
  cargo install clitest@$CLITEST_VERSION
  DEBUG_TARGET=`pwd`/target/debug/

  ARGS="{{ARGS}}"
  if [ -z "$ARGS" ]; then
    echo "Running all tests..."
    echo ' 🔎 Re-run with `just clitest <test1> <test2>` to see detailed output.'
    find tests/scripts -name "*.cli" -type f | while read -r test_file; do
      test_name=$(basename "$test_file" .cli)
      PATH=$DEBUG_TARGET:$PATH clitest --quiet --timeout 120 "$test_file"
    done
  else
    echo "Running tests: $ARGS"
    for test_file in $ARGS; do
      test_name=$(basename "$test_file" .cli)
      PATH=$DEBUG_TARGET:$PATH clitest --timeout 120 "$test_file"
    done
  fi
