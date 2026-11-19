//! The language (`hl`) and country (`gl`) YouTube is asked to answer in: the titles,
//! names and labels of songs, artists and albums. Not the app's own text (`i18n`).

/// What every request carrying a context asks for. Copy, so it's read without a lock held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Locale {
    pub hl: &'static str,
    pub gl: &'static str,
}

/// The default, and what the requests whose answers are matched as English text keep.
pub const ENGLISH: Locale = Locale { hl: "en", gl: "US" };

impl Default for Locale {
    fn default() -> Self {
        ENGLISH
    }
}

impl Locale {
    /// From saved codes; one YouTube doesn't list falls back to the default.
    pub fn from_codes(hl: Option<&str>, gl: Option<&str>) -> Self {
        Self {
            hl: hl.and_then(language).map_or(ENGLISH.hl, |l| l.0),
            gl: gl.and_then(country).map_or(ENGLISH.gl, |c| c.0),
        }
    }
}

/// How a language is listed: its English name, then its own when that differs.
pub fn language_label(l: &(&str, &str, &str)) -> String {
    if l.2.is_empty() || l.1 == l.2 { l.1.to_owned() } else { format!("{} ({})", l.1, l.2) }
}

pub fn language(hl: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    LANGUAGES.iter().find(|l| l.0 == hl)
}

pub fn country(gl: &str) -> Option<&'static (&'static str, &'static str)> {
    COUNTRIES.iter().find(|c| c.0 == gl)
}

/// The languages YouTube offers: code, English name, own name.
pub const LANGUAGES: &[(&str, &str, &str)] = &[
    ("af", "Afrikaans", "Afrikaans"),
    ("am", "Amharic", "አማርኛ"),
    ("ar", "Arabic", "العربية"),
    ("as", "Assamese", "অসমীয়া"),
    ("az", "Azerbaijani", "Azərbaycan"),
    ("be", "Belarusian", "Беларуская"),
    ("bg", "Bulgarian", "Български"),
    ("bn", "Bangla", "বাংলা"),
    ("bs", "Bosnian", "Bosanski"),
    ("ca", "Catalan", "Català"),
    ("cs", "Czech", "Čeština"),
    ("da", "Danish", "Dansk"),
    ("de", "German", "Deutsch"),
    ("el", "Greek", "Ελληνικά"),
    ("en", "English (US)", ""),
    ("en-GB", "English (UK)", ""),
    ("en-IN", "English (India)", ""),
    ("es", "Spanish (Spain)", "Español (España)"),
    ("es-419", "Spanish (Latin America)", "Español (Latinoamérica)"),
    ("es-US", "Spanish (US)", "Español (US)"),
    ("et", "Estonian", "Eesti"),
    ("eu", "Basque", "Euskara"),
    ("fa", "Persian", "فارسی"),
    ("fi", "Finnish", "Suomi"),
    ("fil", "Filipino", "Filipino"),
    ("fr", "French", "Français"),
    ("fr-CA", "French (Canada)", "Français (Canada)"),
    ("gl", "Galician", "Galego"),
    ("gu", "Gujarati", "ગુજરાતી"),
    ("hi", "Hindi", "हिन्दी"),
    ("hr", "Croatian", "Hrvatski"),
    ("hu", "Hungarian", "Magyar"),
    ("hy", "Armenian", "Հայերեն"),
    ("id", "Indonesian", "Bahasa Indonesia"),
    ("is", "Icelandic", "Íslenska"),
    ("it", "Italian", "Italiano"),
    ("iw", "Hebrew", "עברית"),
    ("ja", "Japanese", "日本語"),
    ("ka", "Georgian", "ქართული"),
    ("kk", "Kazakh", "Қазақ тілі"),
    ("km", "Khmer", "ខ្មែរ"),
    ("kn", "Kannada", "ಕನ್ನಡ"),
    ("ko", "Korean", "한국어"),
    ("ky", "Kyrgyz", "Кыргызча"),
    ("lo", "Lao", "ລາວ"),
    ("lt", "Lithuanian", "Lietuvių"),
    ("lv", "Latvian", "Latviešu"),
    ("mk", "Macedonian", "Македонски"),
    ("ml", "Malayalam", "മലയാളം"),
    ("mn", "Mongolian", "Монгол"),
    ("mr", "Marathi", "मराठी"),
    ("ms", "Malay", "Bahasa Malaysia"),
    ("my", "Burmese", "ဗမာ"),
    ("ne", "Nepali", "नेपाली"),
    ("nl", "Dutch", "Nederlands"),
    ("no", "Norwegian", "Norsk"),
    ("or", "Odia", "ଓଡ଼ିଆ"),
    ("pa", "Punjabi", "ਪੰਜਾਬੀ"),
    ("pl", "Polish", "Polski"),
    ("pt", "Portuguese (Brazil)", "Português (Brasil)"),
    ("pt-PT", "Portuguese (Portugal)", "Português"),
    ("ro", "Romanian", "Română"),
    ("ru", "Russian", "Русский"),
    ("si", "Sinhala", "සිංහල"),
    ("sk", "Slovak", "Slovenčina"),
    ("sl", "Slovenian", "Slovenščina"),
    ("sq", "Albanian", "Shqip"),
    ("sr", "Serbian", "Српски"),
    ("sr-Latn", "Serbian (Latin)", "Srpski"),
    ("sv", "Swedish", "Svenska"),
    ("sw", "Swahili", "Kiswahili"),
    ("ta", "Tamil", "தமிழ்"),
    ("te", "Telugu", "తెలుగు"),
    ("th", "Thai", "ไทย"),
    ("tr", "Turkish", "Türkçe"),
    ("uk", "Ukrainian", "Українська"),
    ("ur", "Urdu", "اردو"),
    ("uz", "Uzbek", "O‘zbek"),
    ("vi", "Vietnamese", "Tiếng Việt"),
    ("zh-CN", "Chinese (Simplified)", "中文 (简体)"),
    ("zh-HK", "Chinese (Hong Kong)", "中文 (香港)"),
    ("zh-TW", "Chinese (Traditional)", "中文 (繁體)"),
    ("zu", "Zulu", "isiZulu"),
];

/// The countries YouTube offers: code, English name.
pub const COUNTRIES: &[(&str, &str)] = &[
    ("AE", "United Arab Emirates"),
    ("AR", "Argentina"),
    ("AT", "Austria"),
    ("AU", "Australia"),
    ("AZ", "Azerbaijan"),
    ("BA", "Bosnia and Herzegovina"),
    ("BD", "Bangladesh"),
    ("BE", "Belgium"),
    ("BG", "Bulgaria"),
    ("BH", "Bahrain"),
    ("BO", "Bolivia"),
    ("BR", "Brazil"),
    ("BY", "Belarus"),
    ("CA", "Canada"),
    ("CH", "Switzerland"),
    ("CL", "Chile"),
    ("CO", "Colombia"),
    ("CR", "Costa Rica"),
    ("CY", "Cyprus"),
    ("CZ", "Czechia"),
    ("DE", "Germany"),
    ("DK", "Denmark"),
    ("DO", "Dominican Republic"),
    ("DZ", "Algeria"),
    ("EC", "Ecuador"),
    ("EE", "Estonia"),
    ("EG", "Egypt"),
    ("ES", "Spain"),
    ("FI", "Finland"),
    ("FR", "France"),
    ("GB", "United Kingdom"),
    ("GE", "Georgia"),
    ("GH", "Ghana"),
    ("GR", "Greece"),
    ("GT", "Guatemala"),
    ("HK", "Hong Kong"),
    ("HN", "Honduras"),
    ("HR", "Croatia"),
    ("HU", "Hungary"),
    ("ID", "Indonesia"),
    ("IE", "Ireland"),
    ("IL", "Israel"),
    ("IN", "India"),
    ("IQ", "Iraq"),
    ("IS", "Iceland"),
    ("IT", "Italy"),
    ("JM", "Jamaica"),
    ("JO", "Jordan"),
    ("JP", "Japan"),
    ("KE", "Kenya"),
    ("KH", "Cambodia"),
    ("KR", "South Korea"),
    ("KW", "Kuwait"),
    ("KZ", "Kazakhstan"),
    ("LA", "Laos"),
    ("LB", "Lebanon"),
    ("LI", "Liechtenstein"),
    ("LK", "Sri Lanka"),
    ("LT", "Lithuania"),
    ("LU", "Luxembourg"),
    ("LV", "Latvia"),
    ("LY", "Libya"),
    ("MA", "Morocco"),
    ("ME", "Montenegro"),
    ("MK", "North Macedonia"),
    ("MT", "Malta"),
    ("MX", "Mexico"),
    ("MY", "Malaysia"),
    ("NG", "Nigeria"),
    ("NI", "Nicaragua"),
    ("NL", "Netherlands"),
    ("NO", "Norway"),
    ("NP", "Nepal"),
    ("NZ", "New Zealand"),
    ("OM", "Oman"),
    ("PA", "Panama"),
    ("PE", "Peru"),
    ("PG", "Papua New Guinea"),
    ("PH", "Philippines"),
    ("PK", "Pakistan"),
    ("PL", "Poland"),
    ("PR", "Puerto Rico"),
    ("PT", "Portugal"),
    ("PY", "Paraguay"),
    ("QA", "Qatar"),
    ("RO", "Romania"),
    ("RS", "Serbia"),
    ("RU", "Russia"),
    ("SA", "Saudi Arabia"),
    ("SE", "Sweden"),
    ("SG", "Singapore"),
    ("SI", "Slovenia"),
    ("SK", "Slovakia"),
    ("SN", "Senegal"),
    ("SV", "El Salvador"),
    ("TH", "Thailand"),
    ("TN", "Tunisia"),
    ("TR", "Turkey"),
    ("TW", "Taiwan"),
    ("TZ", "Tanzania"),
    ("UA", "Ukraine"),
    ("UG", "Uganda"),
    ("US", "United States"),
    ("UY", "Uruguay"),
    ("VE", "Venezuela"),
    ("VN", "Vietnam"),
    ("YE", "Yemen"),
    ("ZA", "South Africa"),
    ("ZW", "Zimbabwe"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_codes_are_read_and_unknown_ones_fall_back_to_english() {
        assert_eq!(Locale::from_codes(Some("ja"), Some("JP")), Locale { hl: "ja", gl: "JP" });
        assert_eq!(Locale::from_codes(Some("klingon"), None), ENGLISH);
        assert_eq!(Locale::from_codes(None, Some("XX")), ENGLISH);
    }

    #[test]
    fn languages_and_countries_are_listed_once_each() {
        let unique = |mut codes: Vec<&str>| {
            let n = codes.len();
            codes.sort_unstable();
            codes.dedup();
            codes.len() == n
        };
        assert!(unique(LANGUAGES.iter().map(|l| l.0).collect()));
        assert!(unique(COUNTRIES.iter().map(|c| c.0).collect()));
        assert_eq!(language_label(language("ja").unwrap()), "Japanese (日本語)");
        assert_eq!(language_label(language("en").unwrap()), "English (US)");
    }
}
