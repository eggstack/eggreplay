# CLI and exit contracts

Every result-producing command accepts `--output human|json`; `test` also
accepts JUnit. JSON is an envelope containing command, schema version,
success, failure class, warnings, and payload. Exit 0 means the command and
its assertion (if any) passed; exit 1 is a fixture, runtime, configuration,
or regression failure. Human output is for people and is not a parsing
contract.
