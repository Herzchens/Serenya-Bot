use regex::Regex;

pub(super) fn extract_player_functions(body: &str) -> (String, String) {
    let mut decipher_js = String::new();
    let mut ncode_js = String::new();

    let decipher_name = between(body, r#"a.set("alr","yes");c&&(c="#, "(decodeURIC");
    if !decipher_name.is_empty() {
        decipher_js = extract_named_function(body, &decipher_name, true).unwrap_or_default();
    }

    let ncode_name = find_ncode_name(body);
    if !ncode_name.is_empty() {
        ncode_js = extract_named_function(body, &ncode_name, false).unwrap_or_default();
    }

    (decipher_js, ncode_js)
}

fn extract_named_function(body: &str, name: &str, include_helper: bool) -> Option<String> {
    let function_start = format!("{name}=function(a)");
    let ndx = body.find(function_start.as_str())?;
    let sub_body = &body[ndx + function_start.len()..];
    let cut_body = cut_after_js(sub_body)?;
    let mut full_body = format!("var {function_start}{cut_body};");

    if include_helper {
        let helper_obj_name = between(cut_body, "a=a.split(\"\");", ".");
        if let Some(helper_body) = extract_helper_object(body, &helper_obj_name) {
            full_body = format!("{helper_body}{full_body}");
        }
    }

    full_body.retain(|c| c != '\n');
    Some(full_body)
}

fn extract_helper_object(body: &str, helper_obj_name: &str) -> Option<String> {
    if helper_obj_name.is_empty() {
        return None;
    }
    let helper_start = format!("var {helper_obj_name}={{");
    let h_ndx = body.find(helper_start.as_str())?;
    let h_sub = &body[h_ndx + helper_start.len() - 1..];
    cut_after_js(h_sub).map(|h_cut| format!("var {helper_obj_name}={h_cut};"))
}

fn find_ncode_name(body: &str) -> String {
    let mut ncode_name = between(body, r#"c=a.get(b))&&(c="#, "(c)");
    if ncode_name.contains('[') {
        let left_name = format!(
            "var {splitted_function_name}=[",
            splitted_function_name = ncode_name.split('[').next().unwrap_or("")
        );
        ncode_name = between(body, left_name.as_str(), "]");
    }
    if ncode_name.is_empty() {
        ncode_name = scan_ncode_function_name(body);
    }
    ncode_name
}

fn scan_ncode_function_name(body: &str) -> String {
    let Ok(re) = Regex::new(r";\s*([a-zA-Z0-9_$]+)\s*=\s*function\([a-zA-Z0-9_$]+\)\s*\{") else {
        return String::new();
    };
    for caps in re.captures_iter(body) {
        let Some(name) = caps.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let Some(full_match) = caps.get(0) else {
            continue;
        };
        let start_pos = full_match.end();
        if let Some(end_pos) = body[start_pos..].find("};") {
            let f_body = &body[start_pos..start_pos + end_pos];
            if f_body.contains("enhanced_except_") {
                return name.to_string();
            }
        }
    }
    String::new()
}

fn between(str: &str, a: &str, b: &str) -> String {
    if let Some(ndx) = str.find(a) {
        let sub = &str[ndx + a.len()..];
        if let Some(end_ndx) = sub.find(b) {
            return sub[..end_ndx].to_string();
        }
    }
    String::new()
}

fn cut_after_js(str: &str) -> Option<&str> {
    if !str.starts_with('{') {
        return None;
    }
    let mut open_braces = 0;
    for (i, c) in str.char_indices() {
        if c == '{' {
            open_braces += 1;
        } else if c == '}' {
            open_braces -= 1;
            if open_braces == 0 {
                return Some(&str[..=i]);
            }
        }
    }
    None
}
