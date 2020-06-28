use lazy_static::lazy_static;

use super::{week2, year1, Matcher, MatcherDef, Year};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CgbStamp {
    pub year: Option<Year>,
    pub week: Option<u8>,
}

/// ```
/// # use gbhwdb_backend::parser::parse_cgb_stamp;
/// assert!(parse_cgb_stamp("218-2221").is_some());
/// ```
fn cgb_stamp() -> MatcherDef<CgbStamp> {
    MatcherDef(r#"^([0-9]{2})([0-9])[-\ .X]?[0-9]{2,4}Y?$"#, move |c| {
        Ok(CgbStamp {
            year: Some(year1(&c[2])?),
            week: Some(week2(&c[1])?),
        })
    })
}

pub fn parse_cgb_stamp(text: &str) -> Option<CgbStamp> {
    lazy_static! {
        static ref MATCHER: Matcher<CgbStamp> = cgb_stamp().into();
    }
    MATCHER.apply(text)
}
