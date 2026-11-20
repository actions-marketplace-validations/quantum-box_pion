//! Checks a plan against the results of a full test run.
//!
//! A test that failed in the full run but was not selected by the plan is a
//! miss: the plan would have let the failure through. Counting misses over
//! time is how the selection rules earn trust.

use serde::Serialize;

use crate::plan::TestPlan;

/// The result of one test in a JUnit report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// The crate, taken from the test binary's id (`package` or
    /// `package::binary`), which nextest writes as the `classname`.
    pub package: String,
    pub test: String,
    pub failed: bool,
}

/// Reads the `<testcase>` elements of a JUnit XML report as nextest writes
/// it. A test case counts as failed when it holds a `<failure>` or `<error>`.
pub fn read_junit(xml: &str) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<testcase") {
        let tag = &rest[start..];
        let Some(tag_end) = tag.find('>') else { break };
        let opening = &tag[..tag_end];
        let (body, after) = if opening.ends_with('/') {
            ("", &tag[tag_end + 1..])
        } else {
            match tag[tag_end + 1..].find("</testcase>") {
                Some(close) => {
                    let body_start = tag_end + 1;
                    let after = body_start + close + "</testcase>".len();
                    (&tag[body_start..body_start + close], &tag[after..])
                }
                None => (&tag[tag_end + 1..], ""),
            }
        };
        let name = attribute(opening, "name");
        let classname = attribute(opening, "classname");
        if let (Some(test), Some(classname)) = (name, classname) {
            let package = classname.split("::").next().unwrap_or("").to_owned();
            outcomes.push(Outcome {
                package,
                test,
                failed: body.contains("<failure") || body.contains("<error"),
            });
        }
        rest = after;
    }
    outcomes
}

/// Reads the output of `cargo test` (libtest), as found in a CI job log.
///
/// ```text
///      Running unittests src/lib.rs (target/debug/deps/fx_auth-0a1b2c3d4e5f6789)
/// test tests::creates_user ... ok
/// test tests::rejects_expired_token ... FAILED
/// ```
///
/// The crate comes from the test binary's file name, where cargo writes `_`
/// for `-`; [`TestPlan::selects`] compares crate names with that in mind.
/// Anything a log viewer puts before these lines (a job name, a timestamp)
/// is ignored. An integration test binary is named after its file, not its
/// crate, so its failures are reported under that name.
pub fn read_libtest(log: &str) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    let mut package = String::new();
    for line in log.lines() {
        if let Some((_, binary)) = line.split_once("Running ") {
            if let Some(name) = test_binary_name(binary) {
                package = name;
            }
            continue;
        }
        let Some((_, result)) = line.split_once("test ") else {
            continue;
        };
        let Some((test, verdict)) = result.split_once(" ... ") else {
            continue;
        };
        let failed = match verdict.trim() {
            "ok" => false,
            "FAILED" => true,
            _ => continue,
        };
        // `test path - should panic ... ok` names the test before the ` - `.
        let test = test.split(" - ").next().unwrap_or(test).trim();
        if !test.is_empty() && !test.contains(' ') {
            outcomes.push(Outcome {
                package: package.clone(),
                test: test.to_owned(),
                failed,
            });
        }
    }
    outcomes
}

/// `fx_auth` from `unittests src/lib.rs (target/debug/deps/fx_auth-0a1b2c3d)`.
fn test_binary_name(running: &str) -> Option<String> {
    let path = running.rsplit_once('(')?.1.split(')').next()?;
    let file = path.rsplit(['/', '\\']).next()?;
    let (name, hash) = file.rsplit_once('-')?;
    let hash = hash.trim_end_matches(".exe");
    (!name.is_empty() && hash.chars().all(|c| c.is_ascii_hexdigit())).then(|| name.to_owned())
}

/// The value of `name="…"` in an opening tag, with XML entities decoded.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let mut search = tag;
    loop {
        let found = search.find(name)?;
        let before = search[..found].chars().next_back();
        let after = &search[found + name.len()..];
        if before.is_some_and(char::is_whitespace) && after.starts_with("=\"") {
            let value = &after[2..];
            let end = value.find('"')?;
            return Some(unescape(&value[..end]));
        }
        search = after;
    }
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The failed tests the plan did not select.
pub fn missed(plan: &TestPlan, outcomes: &[Outcome]) -> Vec<Outcome> {
    outcomes
        .iter()
        .filter(|outcome| outcome.failed && !plan.selects(&outcome.package, &outcome.test))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="1">
  <testsuite name="fx-auth" tests="2" failures="1">
    <testcase name="tests::creates_user" classname="fx-auth" time="0.01"/>
    <testcase name="tests::rejects_expired_token" classname="fx-auth" time="0.02">
      <failure message="assertion &quot;left == right&quot; failed">boom</failure>
    </testcase>
  </testsuite>
  <testsuite name="fx-auth::flow" tests="1">
    <testcase name="sign_up_flow" classname="fx-auth::flow" time="0.01">
      <system-out>ok</system-out>
    </testcase>
  </testsuite>
</testsuites>"#;

    #[test]
    fn reads_each_test_case_and_whether_it_failed() {
        let outcome = |package: &str, test: &str, failed| Outcome {
            package: package.into(),
            test: test.into(),
            failed,
        };

        assert_eq!(
            read_junit(REPORT),
            [
                outcome("fx-auth", "tests::creates_user", false),
                outcome("fx-auth", "tests::rejects_expired_token", true),
                outcome("fx-auth", "sign_up_flow", false),
            ]
        );
    }

    #[test]
    fn reads_cargo_test_output_from_a_job_log() {
        let log = "\
test-auth\tRun tests\t2026-11-20T12:00:00.0000000Z      Running unittests src/lib.rs (target/debug/deps/fx_auth-0a1b2c3d4e5f6789)
test-auth\tRun tests\t2026-11-20T12:00:01.0000000Z running 3 tests
test-auth\tRun tests\t2026-11-20T12:00:01.0000000Z test tests::creates_user ... ok
test-auth\tRun tests\t2026-11-20T12:00:01.0000000Z test tests::rejects_expired_token ... FAILED
test-auth\tRun tests\t2026-11-20T12:00:01.0000000Z test tests::panics - should panic ... ok
test-auth\tRun tests\t2026-11-20T12:00:01.0000000Z test tests::slow ... ignored
test-auth\tRun tests\t2026-11-20T12:00:02.0000000Z test result: FAILED. 2 passed; 1 failed; 1 ignored
test-auth\tRun tests\t2026-11-20T12:00:03.0000000Z      Running tests/flow.rs (target/debug/deps/flow-fedcba9876543210)
test-auth\tRun tests\t2026-11-20T12:00:03.0000000Z test sign_up_flow ... ok
";
        let outcome = |package: &str, test: &str, failed| Outcome {
            package: package.into(),
            test: test.into(),
            failed,
        };

        assert_eq!(
            read_libtest(log),
            [
                outcome("fx_auth", "tests::creates_user", false),
                outcome("fx_auth", "tests::rejects_expired_token", true),
                outcome("fx_auth", "tests::panics", false),
                outcome("flow", "sign_up_flow", false),
            ]
        );
    }

    #[test]
    fn a_failure_the_plan_did_not_select_is_a_miss() {
        let outcomes = read_junit(REPORT);
        let empty = TestPlan::default();

        let misses = missed(&empty, &outcomes);

        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].test, "tests::rejects_expired_token");
    }
}
