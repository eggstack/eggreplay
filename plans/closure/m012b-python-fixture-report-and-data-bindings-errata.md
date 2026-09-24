# M012B Closure Errata — Lifetime Stress Coverage

The original M012B closure at
`m012b-python-fixture-report-and-data-bindings.md` recorded one local skip as
unavailable symlink creation. Review found the skip was actually misplaced at
the end of the repeated fixture-child GC test, so only its first loop iteration
ran. The symlink test also did not assert rejection when link creation worked.

The Python test harness was corrected in revision
`9c71c367fedb8e3aa45aa68bde10f9d45931bf38`: the GC test now completes all 32
fixture/iterator/flow/body-reader lifetimes, and the symlink test asserts
`FixtureError` when symlink creation is available, otherwise skips that test
explicitly after restoring the fixture blob.

Hosted qualification passed with all 19 Python tests in every binding lane on
[GitHub Actions run 35959113753](https://github.com/eggstack/eggreplay/actions/runs/35959113753).
The 19 tests passed on Ubuntu CPython 3.11 and 3.14, macOS CPython 3.11, and
Windows CPython 3.11. This errata supplements the historical M012B closure;
its original evidence record remains unchanged.
