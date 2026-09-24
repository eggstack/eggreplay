use pyo3::create_exception;

create_exception!(_native, EggReplayError, pyo3::exceptions::PyException);
create_exception!(_native, FixtureError, EggReplayError);
create_exception!(_native, MatchError, EggReplayError);
create_exception!(_native, NetworkError, EggReplayError);
create_exception!(_native, ConfigurationError, EggReplayError);
create_exception!(_native, RegressionError, EggReplayError);

pub fn fixture_error(_: impl std::fmt::Display) -> pyo3::PyErr {
    FixtureError::new_err("fixture is invalid or unreadable")
}
