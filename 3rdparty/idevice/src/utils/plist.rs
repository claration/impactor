/// Utilities for working with plist values
///
/// Truncates all Date values in a plist structure to second precision.
///
/// This function recursively walks through a plist Value and truncates any Date values
/// from nanosecond precision to second precision. This is necessary for compatibility
/// with iOS devices that reject high-precision date formats.
///
/// # Arguments
/// * `value` - The plist Value to normalize (modified in place)
///
/// # Example
/// ```rust,no_run
/// use idevice::utils::plist::truncate_dates_to_seconds;
/// use plist::Value;
///
/// let mut icon_state = Value::Array(vec![]);
/// truncate_dates_to_seconds(&mut icon_state);
/// ```
///
/// # Details
/// - Converts dates from format: `2026-01-17T03:09:58.332738876Z` (nanosecond precision)
/// - To format: `2026-01-17T03:09:58Z` (second precision)
/// - Recursively processes Arrays and Dictionaries
/// - Other value types are left unchanged
pub fn truncate_dates_to_seconds(value: &mut plist::Value) {
    match value {
        plist::Value::Date(date) => {
            let xml_string = date.to_xml_format();
            if let Some(dot_pos) = xml_string.find('.')
                && xml_string[dot_pos..].contains('Z')
            {
                let truncated_string = format!("{}Z", &xml_string[..dot_pos]);
                if let Ok(new_date) = plist::Date::from_xml_format(&truncated_string) {
                    *date = new_date;
                }
            }
        }
        plist::Value::Array(arr) => {
            for item in arr.iter_mut() {
                truncate_dates_to_seconds(item);
            }
        }
        plist::Value::Dictionary(dict) => {
            for (_, v) in dict.iter_mut() {
                truncate_dates_to_seconds(v);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_date_with_nanoseconds() {
        let date_str = "2026-01-17T03:09:58.332738876Z";
        let date = plist::Date::from_xml_format(date_str).unwrap();
        let mut value = plist::Value::Date(date);

        truncate_dates_to_seconds(&mut value);

        if let plist::Value::Date(truncated_date) = value {
            let result = truncated_date.to_xml_format();
            assert!(
                !result.contains('.'),
                "Date should not contain fractional seconds"
            );
            assert!(result.ends_with('Z'), "Date should end with Z");
            assert!(
                result.starts_with("2026-01-17T03:09:58"),
                "Date should preserve main timestamp"
            );
        } else {
            panic!("Value should still be a Date");
        }
    }

    #[test]
    fn test_truncate_date_already_truncated() {
        let date_str = "2026-01-17T03:09:58Z";
        let date = plist::Date::from_xml_format(date_str).unwrap();
        let original_format = date.to_xml_format();
        let mut value = plist::Value::Date(date);

        truncate_dates_to_seconds(&mut value);

        if let plist::Value::Date(truncated_date) = value {
            let result = truncated_date.to_xml_format();
            assert_eq!(
                result, original_format,
                "Already truncated date should remain unchanged"
            );
        }
    }

    #[test]
    fn test_truncate_dates_in_array() {
        let date1 = plist::Date::from_xml_format("2026-01-17T03:09:58.123456Z").unwrap();
        let date2 = plist::Date::from_xml_format("2026-01-18T04:10:59.987654Z").unwrap();
        let mut value =
            plist::Value::Array(vec![plist::Value::Date(date1), plist::Value::Date(date2)]);

        truncate_dates_to_seconds(&mut value);

        if let plist::Value::Array(arr) = value {
            for item in arr {
                if let plist::Value::Date(date) = item {
                    let formatted = date.to_xml_format();
                    assert!(
                        !formatted.contains('.'),
                        "Dates in array should be truncated"
                    );
                }
            }
        }
    }

    #[test]
    fn test_truncate_dates_in_dictionary() {
        let date = plist::Date::from_xml_format("2026-01-17T03:09:58.999999Z").unwrap();
        let mut dict = plist::Dictionary::new();
        dict.insert("timestamp".to_string(), plist::Value::Date(date));
        let mut value = plist::Value::Dictionary(dict);

        truncate_dates_to_seconds(&mut value);

        if let plist::Value::Dictionary(dict) = value
            && let Some(plist::Value::Date(date)) = dict.get("timestamp")
        {
            let formatted = date.to_xml_format();
            assert!(
                !formatted.contains('.'),
                "Date in dictionary should be truncated"
            );
        }
    }

    #[test]
    fn test_other_value_types_unchanged() {
        let mut string_val = plist::Value::String("test".to_string());
        let mut int_val = plist::Value::Integer(42.into());
        let mut bool_val = plist::Value::Boolean(true);

        truncate_dates_to_seconds(&mut string_val);
        truncate_dates_to_seconds(&mut int_val);
        truncate_dates_to_seconds(&mut bool_val);

        assert!(matches!(string_val, plist::Value::String(_)));
        assert!(matches!(int_val, plist::Value::Integer(_)));
        assert!(matches!(bool_val, plist::Value::Boolean(_)));
    }
}
