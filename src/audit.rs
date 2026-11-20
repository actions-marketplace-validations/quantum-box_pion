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
    fn a_failure_the_plan_did_not_select_is_a_miss() {
        let outcomes = read_junit(REPORT);
        let empty = TestPlan::default();

        let misses = missed(&empty, &outcomes);

        assert_eq!(misses.len(), 1);
        assert_eq!(misses[0].test, "tests::rejects_expired_token");
    }
}
