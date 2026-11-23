// Jackson Coxson

#[macro_export]
macro_rules! obf {
    ($lit:literal) => {{
        #[cfg(feature = "obfuscate")]
        {
            std::borrow::Cow::Owned(obfstr::obfstr!($lit).to_string())
        }
        #[cfg(not(feature = "obfuscate"))]
        {
            std::borrow::Cow::Borrowed($lit)
        }
    }};
}
