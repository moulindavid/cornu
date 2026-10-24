const MAX_LEN: usize = 60;

pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = fold_accent(c);
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = match slug.char_indices().nth(MAX_LEN) {
        Some((cut, _)) => slug[..cut].trim_end_matches('-'),
        None => slug,
    };
    if slug.is_empty() {
        "untitled".to_owned()
    } else {
        slug.to_owned()
    }
}

fn fold_accent(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_lowercase_and_dashed() {
        assert_eq!(slugify("Get user by ID"), "get-user-by-id");
        assert_eq!(slugify("  /users/{id}  "), "users-id");
        assert_eq!(slugify("Récupérer l'élève"), "recuperer-l-eleve");
    }

    #[test]
    fn empty_or_symbol_names_get_a_fallback() {
        assert_eq!(slugify(""), "untitled");
        assert_eq!(slugify("🚀 !!"), "untitled");
    }

    #[test]
    fn long_names_are_truncated_without_trailing_dash() {
        let slug = slugify(&"ab ".repeat(40));
        assert!(slug.len() <= MAX_LEN);
        assert!(!slug.ends_with('-'));
    }
}
