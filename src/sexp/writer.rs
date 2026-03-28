use super::Ingot;

/// Serialize an Ingot back to its s-expression string representation.
pub fn write_ingot(ingot: &Ingot) -> String {
    let solo = if ingot.solo { "t" } else { "nil" };
    let id = escape_quoted(&ingot.id);
    let proof = escape_quoted(&ingot.proof);
    let work = escape_quoted(&ingot.work);
    let mut s = format!(
        "(ingot :id \"{}\" :status {} :solo {} :grade {} :skill {} :heat {} :max {} :smelt {} :proof \"{}\" :work \"{}\"",
        id,
        ingot.status,
        solo,
        ingot.grade,
        ingot.skill,
        ingot.heat,
        ingot.max,
        ingot.smelt,
        proof,
        work,
    );

    // Append budget if present
    if let Some(budget) = ingot.budget {
        s.push_str(&format!(" :budget {budget}"));
    }

    // Append unknown extra fields for forward compatibility
    for (key, value) in &ingot.extra {
        // If value looks like it needs quoting (contains spaces), quote it
        if value.contains(' ') || value.contains('"') {
            s.push_str(&format!(" :{key} \"{}\"", escape_quoted(value)));
        } else {
            s.push_str(&format!(" :{key} {value}"));
        }
    }

    s.push(')');
    s
}

fn escape_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::{Skill, Status};

    #[test]
    fn write_basic_ingot() {
        let ingot = Ingot {
            id: "i1".into(),
            status: Status::Ore,
            solo: true,
            grade: 2,
            skill: Skill::Web,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: "test -f index.html".into(),
            work: "Create HTML structure".into(),
            budget: None,
            extra: vec![],
        };
        let s = write_ingot(&ingot);
        assert!(s.starts_with("(ingot "));
        assert!(s.ends_with(')'));
        assert!(s.contains(":id \"i1\""));
        assert!(s.contains(":status ore"));
        assert!(s.contains(":solo t"));
        assert!(s.contains(":grade 2"));
        assert!(s.contains(":skill web"));
    }

    #[test]
    fn write_sequential_ingot() {
        let ingot = Ingot {
            id: "i5".into(),
            status: Status::Cracked,
            solo: false,
            grade: 4,
            skill: Skill::Cli,
            heat: 6,
            max: 8,
            smelt: 1,
            proof: "npm test".into(),
            work: "Deploy app".into(),
            budget: None,
            extra: vec![],
        };
        let s = write_ingot(&ingot);
        assert!(s.contains(":solo nil"));
        assert!(s.contains(":status cracked"));
        assert!(s.contains(":smelt 1"));
    }

    #[test]
    fn write_preserves_extra_fields() {
        let ingot = Ingot {
            id: "i1".into(),
            status: Status::Ore,
            solo: true,
            grade: 1,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: "true".into(),
            work: "test".into(),
            budget: None,
            extra: vec![("custom".into(), "hello".into())],
        };
        let s = write_ingot(&ingot);
        assert!(s.contains(":custom hello"));
    }

    #[test]
    fn write_escapes_quotes_and_backslashes() {
        let ingot = Ingot {
            id: "i1".into(),
            status: Status::Ore,
            solo: true,
            grade: 1,
            skill: Skill::Default,
            heat: 0,
            max: 5,
            smelt: 0,
            proof: "grep -q 'A\\|B'".into(),
            work: "He said \"ok\"".into(),
            budget: None,
            extra: vec![],
        };
        let s = write_ingot(&ingot);
        assert!(s.contains("A\\\\|B"));
        assert!(s.contains("He said \\\"ok\\\""));
    }
}
