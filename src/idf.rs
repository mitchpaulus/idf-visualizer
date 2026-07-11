//! Minimal IDF parser: splits a file into objects with their fields,
//! keeping the raw source text of each object for inspection.

#[derive(Debug, Clone)]
pub struct IdfObject {
    /// Object class, e.g. "BuildingSurface:Detailed" (original casing preserved).
    pub class: String,
    /// Fields after the class name. Trimmed, comments stripped. May be empty strings.
    pub fields: Vec<String>,
    /// 1-based line number where the object starts.
    pub line: usize,
    /// Raw source text of the object (original lines, including comments).
    pub raw: String,
}

impl IdfObject {
    pub fn field(&self, i: usize) -> &str {
        self.fields.get(i).map(String::as_str).unwrap_or("")
    }

    pub fn field_f64(&self, i: usize) -> Option<f64> {
        self.field(i).parse::<f64>().ok()
    }
}

pub fn parse(source: &str) -> Vec<IdfObject> {
    let mut objects = Vec::new();

    // Current object accumulation state.
    let mut fields: Vec<String> = Vec::new();
    let mut pending = String::new(); // field text being accumulated
    let mut raw_lines: Vec<&str> = Vec::new();
    let mut start_line = 0usize;

    for (line_idx, line) in source.lines().enumerate() {
        let code = match line.find('!') {
            Some(pos) => &line[..pos],
            None => line,
        };
        if code.trim().is_empty() {
            continue;
        }
        if fields.is_empty() && pending.trim().is_empty() {
            start_line = line_idx + 1;
            raw_lines.clear();
        }
        raw_lines.push(line);

        for ch in code.chars() {
            match ch {
                ',' => {
                    fields.push(pending.trim().to_string());
                    pending.clear();
                }
                ';' => {
                    fields.push(pending.trim().to_string());
                    pending.clear();
                    if !fields.is_empty() && !fields[0].is_empty() {
                        let class = fields.remove(0);
                        objects.push(IdfObject {
                            class,
                            fields: std::mem::take(&mut fields),
                            line: start_line,
                            raw: raw_lines.join("\n"),
                        });
                    }
                    fields.clear();
                }
                _ => pending.push(ch),
            }
        }
    }

    objects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_object() {
        let src = "Version,\n  25.2;\n";
        let objs = parse(src);
        assert_eq!(objs.len(), 1);
        assert_eq!(objs[0].class, "Version");
        assert_eq!(objs[0].fields, vec!["25.2"]);
    }

    #[test]
    fn strips_comments_and_handles_multiline() {
        let src = "\
! header comment
BuildingSurface:Detailed,
  Wall 1,   !- Name
  Wall,     !- Surface Type
  C1, Z1, S1, Outdoors, , SunExposed, WindExposed, ,
  ,
  0, 0, 0,
  1, 0, 0,
  1, 0, 1;
";
        let objs = parse(src);
        assert_eq!(objs.len(), 1);
        let o = &objs[0];
        assert_eq!(o.class, "BuildingSurface:Detailed");
        assert_eq!(o.field(0), "Wall 1");
        assert_eq!(o.field(1), "Wall");
        assert_eq!(o.fields.len(), 11 + 9);
        assert_eq!(o.field_f64(11), Some(0.0));
        assert!(o.raw.contains("!- Name"));
        assert_eq!(o.line, 2);
    }

    #[test]
    fn empty_fields_preserved() {
        let objs = parse("Foo,a,,c;");
        assert_eq!(objs[0].fields, vec!["a", "", "c"]);
    }
}
