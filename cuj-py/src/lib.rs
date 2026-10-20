//! Python bindings for cuj via PyO3 / maturin.
//!
//! The Rust extension module `cuj._cuj`; the user-facing package
//! is `cuj` (PyPI dist: cuj). One entry point, mirroring the CLI's
//! `cuj q`: [`q`] runs a Quarb expression over the vault arbor and
//! returns typed results — node results as cuj's own display
//! strings (`--default--12`, `///wiki/note.md`, `..tag`), value
//! results as Python values (`int`, `float`, `datetime`,
//! `timedelta`, dicts for records; quantities as their display
//! string, `"204.808_kB"`).
//!
//! Errors surface as Python `ValueError` with cuj's or the
//! engine's message — no envelopes. The invocation instant for
//! `now()` is always bound: one clock read per call, or the `now=`
//! argument to pin it (the `--now` flag's twin, for reproducible
//! queries).

use pyo3::IntoPyObjectExt;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use cuj::app::{App, Opts};
use cuj::arbor::VaultArbor;
use cuj::client::ClientConfig;
use quarb::{QueryResult, Value};

fn value_to_py(py: Python<'_>, v: &Value) -> PyResult<Py<PyAny>> {
    Ok(match v {
        Value::Null => py.None(),
        Value::Bool(b) => b.into_py_any(py)?,
        Value::Int(i) => i.into_py_any(py)?,
        Value::Float(f) => f.into_py_any(py)?,
        Value::Str(s) => s.into_py_any(py)?,
        Value::List(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(value_to_py(py, item)?)?;
            }
            list.unbind().into()
        }
        Value::Record(fields) => {
            let dict = PyDict::new(py);
            for (k, val) in fields {
                dict.set_item(k, value_to_py(py, val)?)?;
            }
            dict.unbind().into()
        }
        Value::Instant {
            secs,
            nanos,
            offset_min,
            ..
        } => {
            // datetime is microsecond-precision; nanos round down.
            // No source offset means the UTC timeline — the result
            // is tz-aware either way.
            let datetime = py.import("datetime")?;
            let tz = match offset_min {
                Some(m) => {
                    let delta = datetime
                        .getattr("timedelta")?
                        .call1((0, i64::from(*m) * 60))?;
                    datetime.getattr("timezone")?.call1((delta,))?
                }
                None => datetime.getattr("timezone")?.getattr("utc")?,
            };
            let dt = datetime
                .getattr("datetime")?
                .call_method1("fromtimestamp", (*secs, &tz))?;
            let kwargs = PyDict::new(py);
            kwargs.set_item("microsecond", nanos / 1000)?;
            dt.call_method("replace", (), Some(&kwargs))?.unbind()
        }
        // A duration is a quantity in a time unit (its durational
        // reading); other quantities keep their written face
        // ("204.808_kB") — a typed Quantity class can come later
        // without breaking this. Periods, tables, nodes and foreign
        // values ride as their display string likewise.
        Value::Quantity { .. } => match v.durational_reading() {
            Some((secs, nanos)) => py
                .import("datetime")?
                .getattr("timedelta")?
                .call1((0, secs, nanos / 1000))?
                .unbind(),
            None => v.to_string().into_py_any(py)?,
        },
        _ => v.to_string().into_py_any(py)?,
    })
}

/// Run a Quarb expression over the vault arbor — `cuj q`, as a
/// function.
///
/// - `config`: path to a cuj config.toml; default is the CLI's own
///   resolution (`CUJ_CONFIG`, then `~/.cuj/config.toml`).
/// - `profile`: operate on the named profile (`"all"` widens).
/// - `at`: a stateref for historical reads (`"~8"`, a hash prefix,
///   an ISO instant) — the `--at` flag.
/// - `now`: an ISO instant pinning `now()` — the `--now` flag;
///   unset, one clock read binds it.
///
/// Node results return as cuj display strings; value results as
/// typed Python values.
#[pyfunction]
#[pyo3(signature = (expr, *, config=None, profile=None, at=None, now=None))]
fn q(
    py: Python<'_>,
    expr: &str,
    config: Option<&str>,
    profile: Option<&str>,
    at: Option<&str>,
    now: Option<&str>,
) -> PyResult<Py<PyAny>> {
    let err = |e: String| PyValueError::new_err(e);
    let client = match config {
        Some(p) => ClientConfig::load_from(p),
        None => ClientConfig::load(),
    }
    .map_err(|e| err(e.to_string()))?;
    let app = App::open(client).map_err(|e| err(e.to_string()))?;
    let opts = Opts {
        profile: profile.map(String::from),
        at: at.map(String::from),
        ..Opts::default()
    };
    let reads = app.reads(&opts).map_err(|e| err(e.to_string()))?;
    let arbor = VaultArbor::build(&reads);
    let (secs, nanos) = match now {
        Some(s) => quarb::temporal::parse_iso(s)
            .map(|(secs, nanos, _)| (secs, nanos))
            .ok_or_else(|| err(format!("bad now instant {s:?}")))?,
        None => {
            let ns = cuj::facts::now_ns();
            (
                ns.div_euclid(1_000_000_000),
                ns.rem_euclid(1_000_000_000) as u32,
            )
        }
    };
    let run = quarb::Run {
        now: Some((secs, nanos)),
        ..quarb::Run::ambient()
    };
    let result = quarb::Query::parse_with(expr, &quarb::Defs::default(), &arbor, &run)
        .and_then(|q| q.run(&arbor, &run))
        .map(|out| out.result)
        .map_err(|e| err(format!("quarb: {e}")))?;
    match result {
        QueryResult::Nodes(nodes) => {
            let list = PyList::empty(py);
            for n in nodes {
                list.append(arbor.display(n))?;
            }
            Ok(list.unbind().into())
        }
        QueryResult::Values(values) => {
            let list = PyList::empty(py);
            for v in &values {
                list.append(value_to_py(py, v)?)?;
            }
            Ok(list.unbind().into())
        }
    }
}

#[pymodule]
fn _cuj(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add_function(wrap_pyfunction!(q, m)?)?;
    Ok(())
}
