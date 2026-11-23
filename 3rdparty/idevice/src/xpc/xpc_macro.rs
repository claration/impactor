// Jackson Coxson
// Ported from serde's json! and the plist! macro

/// Construct an `XPCObject` from a JSON-like literal.
///
/// The `xpc!` macro allows you to construct `XPCObject` values with a syntax
/// similar to JSON. It supports signed and unsigned integers, which is a key
/// feature of XPC.
///
/// ```
/// # use my_crate::xpc; // Replace with your crate name
/// # use my_crate::xpc_macro::*;
/// #
/// let value = xpc!({
///     "message": "hello",
///     "is_reply": true,
///     "signed_value": -42,
///     "unsigned_value": 42u64,
///     "payload": {
///         "items": [1, 2, 3],
///         "metadata": null // Becomes an empty string
///     }
/// });
/// ```
///
/// ### Interpolation
/// You can interpolate variables and expressions directly into the macro.
/// Any interpolated value must implement the `XpcConvertible` trait.
///
/// ```
/// # use my_crate::xpc;
/// # use my_crate::xpc_macro::*;
/// #
/// let user_id = 1001u64;
/// let is_admin = false;
///
/// let request = xpc!({
///     "user_id": user_id,
///     "is_admin": is_admin,
///     "permissions": ["read", "write"],
/// });
/// ```
///
/// ### Optional Fields and Merging
/// The macro supports optional fields using `?` and dictionary merging
/// using `:<` for cleaner construction of complex objects.
///
/// ```
/// # use my_crate::xpc;
/// # use my_crate::xpc_macro::*;
/// #
/// let maybe_tag: Option<&str> = Some("important");
/// let base_config = xpc!({ "timeout": 5000u64 });
///
/// let message = xpc!({
///     "message_id": "msg-123",
///     "tag":? maybe_tag,
///     :< base_config,
/// });
/// ```
#[macro_export]
macro_rules! xpc {
    // Hide distracting implementation details from the generated rustdoc.
    ($($xpc:tt)+) => {
        $crate::xpc_internal!($($xpc)+)
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! xpc_internal {
    //////////////////////////////////////////////////////////////////////////
    // TT muncher for parsing the inside of an array [...].
    //////////////////////////////////////////////////////////////////////////

    // Done with trailing comma.
    (@array [$($elems:expr,)*]) => {
        vec![$($elems,)*]
    };

    // Done without trailing comma.
    (@array [$($elems:expr),*]) => {
        vec![$($elems),*]
    };

    // Next element is `null`.
    (@array [$($elems:expr,)*] null $($rest:tt)*) => {
        $crate::xpc_internal!(@array [$($elems,)* $crate::xpc_internal!(null)] $($rest)*)
    };

    // Next element is `true`.
    (@array [$($elems:expr,)*] true $($rest:tt)*) => {
        $crate::xpc_internal!(@array [$($elems,)* $crate::xpc_internal!(true)] $($rest)*)
    };

    // Next element is `false`.
    (@array [$($elems:expr,)*] false $($rest:tt)*) => {
        $crate::xpc_internal!(@array [$($elems,)* $crate::xpc_internal!(false)] $($rest)*)
    };

    // Next element is an array.
    (@array [$($elems:expr,)*] [$($array:tt)*] $($rest:tt)*) => {
        $crate::xpc_internal!(@array [$($elems,)* $crate::xpc_internal!([$($array)*])] $($rest)*)
    };

    // Next element is a map.
    (@array [$($elems:expr,)*] {$($map:tt)*} $($rest:tt)*) => {
        $crate::xpc_internal!(@array [$($elems,)* $crate::xpc_internal!({$($map)*})] $($rest)*)
    };

    // Optional element
    (@array [$($elems:expr,)*] ? $maybe:expr, $($rest:tt)*) => {
        if let Some(__v) = $crate::xpc::xpc_macro::xpc_maybe($maybe) {
            $crate::xpc_internal!(@array [$($elems,)* __v,] $($rest)*)
        } else {
            $crate::xpc_internal!(@array [$($elems,)*] $($rest)*)
        }
    };
    (@array [$($elems:expr,)*] ? $maybe:expr) => {
        if let Some(__v) = $crate::xpc_macro::xpc_maybe($maybe) {
            $crate::xpc_internal!(@array [$($elems,)* __v])
        } else {
            $crate::xpc_internal!(@array [$($elems,)*])
        }
    };

    // Next element is an expression followed by comma.
    (@array [$($elems:expr,)*] $next:expr, $($rest:tt)*) => {
        $crate::xpc_internal!(@array [$($elems,)* $crate::xpc_internal!($next),] $($rest)*)
    };

    // Last element is an expression with no trailing comma.
    (@array [$($elems:expr,)*] $last:expr) => {
        $crate::xpc_internal!(@array [$($elems,)* $crate::xpc_internal!($last)])
    };

    // Comma after the most recent element.
    (@array [$($elems:expr),*] , $($rest:tt)*) => {
        $crate::xpc_internal!(@array [$($elems,)*] $($rest)*)
    };

    // Unexpected token after most recent element.
    (@array [$($elems:expr),*] $unexpected:tt $($rest:tt)*) => {
        $crate::xpc_unexpected!($unexpected)
    };

    //////////////////////////////////////////////////////////////////////////
    // TT muncher for parsing the inside of an object {...}.
    //////////////////////////////////////////////////////////////////////////

    // Done.
    (@object $object:ident () () ()) => {};

    // Insert the current entry followed by trailing comma.
    (@object $object:ident [$($key:tt)+] ($value:expr) , $($rest:tt)*) => {
        let _ = $object.insert(($($key)+).into(), $value);
        $crate::xpc_internal!(@object $object () ($($rest)*) ($($rest)*));
    };

    // Current entry followed by unexpected token.
    (@object $object:ident [$($key:tt)+] ($value:expr) $unexpected:tt $($rest:tt)*) => {
        $crate::xpc_unexpected!($unexpected);
    };

    // Insert the last entry without trailing comma.
    (@object $object:ident [$($key:tt)+] ($value:expr)) => {
        let _ = $object.insert(($($key)+).into(), $value);
    };

    // Next value is `null`.
    (@object $object:ident ($($key:tt)+) (: null $($rest:tt)*) $copy:tt) => {
        $crate::xpc_internal!(@object $object [$($key)+] ($crate::xpc_internal!(null)) $($rest)*);
    };

    // Next value is `true`.
    (@object $object:ident ($($key:tt)+) (: true $($rest:tt)*) $copy:tt) => {
        $crate::xpc_internal!(@object $object [$($key)+] ($crate::xpc_internal!(true)) $($rest)*);
    };

    // Next value is `false`.
    (@object $object:ident ($($key:tt)+) (: false $($rest:tt)*) $copy:tt) => {
        $crate::xpc_internal!(@object $object [$($key)+] ($crate::xpc_internal!(false)) $($rest)*);
    };

    // Next value is an array.
    (@object $object:ident ($($key:tt)+) (: [$($array:tt)*] $($rest:tt)*) $copy:tt) => {
        $crate::xpc_internal!(@object $object [$($key)+] ($crate::xpc_internal!([$($array)*])) $($rest)*);
    };

    // Next value is a map.
    (@object $object:ident ($($key:tt)+) (: {$($map:tt)*} $($rest:tt)*) $copy:tt) => {
        $crate::xpc_internal!(@object $object [$($key)+] ($crate::xpc_internal!({$($map)*})) $($rest)*);
    };

    // Optional insert: `key:? value`
    (@object $object:ident ($($key:tt)+) (:? $value:expr, $($rest:tt)*) $copy:tt) => {
        if let Some(__v) = $crate::xpc::xpc_macro::xpc_maybe($value) {
            let _ = $object.insert(($($key)+).into(), __v);
        }
        $crate::xpc_internal!(@object $object () ($($rest)*) ($($rest)*));
    };
    (@object $object:ident ($($key:tt)+) (:? $value:expr) $copy:tt) => {
        if let Some(__v) = $crate::xpc_macro::xpc_maybe($value) {
            let _ = $object.insert(($($key)+).into(), __v);
        }
    };

    // Merge: `:< value`
    (@object $object:ident () (:< $value:expr, $($rest:tt)*) $copy:tt) => {
        {
            let __v = $crate::xpc_internal!($value);
            let __dict = $crate::xpc::xpc_macro::IntoXpcDict::into_xpc_dict(__v);
            for (__k, __val) in __dict {
                let _ = $object.insert(__k, __val);
            }
        }
        $crate::xpc_internal!(@object $object () ($($rest)*) ($($rest)*));
    };
    (@object $object:ident () (:< $value:expr) $copy:tt) => {
        {
            let __v = $crate::xpc_internal!($value);
            let __dict = $crate::xpc_macro::IntoXpcDict::into_xpc_dict(__v);
            for (__k, __val) in __dict {
                let _ = $object.insert(__k, __val);
            }
        }
    };

    // Optional merge: `:<? value`
    (@object $object:ident () (:< ? $value:expr, $($rest:tt)*) $copy:tt) => {
        if let Some(__dict) = $crate::xpc::xpc_macro::maybe_into_xpc_dict($value) {
            for (__k, __val) in __dict {
                let _ = $object.insert(__k, __val);
            }
        }
        $crate::xpc_internal!(@object $object () ($($rest)*) ($($rest)*));
    };
    (@object $object:ident () (:< ? $value:expr) $copy:tt) => {
        if let Some(__dict) = $crate::xpc_macro::maybe_into_xpc_dict($value) {
            for (__k, __val) in __dict {
                let _ = $object.insert(__k, __val);
            }
        }
    };

    // Next value is an expression followed by comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr, $($rest:tt)*) $copy:tt) => {
        $crate::xpc_internal!(@object $object [$($key)+] ($crate::xpc_internal!($value)), $($rest)*);
    };

    // Last value is an expression with no trailing comma.
    (@object $object:ident ($($key:tt)+) (: $value:expr) $copy:tt) => {
        $crate::xpc_internal!(@object $object [$($key)+] ($crate::xpc_internal!($value)));
    };

    // Missing value for last entry.
    (@object $object:ident ($($key:tt)+) (:) $copy:tt) => {
        $crate::xpc_internal!();
    };

    // Missing colon and value for last entry.
    (@object $object:ident ($($key:tt)+) () $copy:tt) => {
        $crate::xpc_internal!();
    };

    // Misplaced colon.
    (@object $object:ident () (: $($rest:tt)*) ($colon:tt $($copy:tt)*)) => {
        $crate::xpc_unexpected!($colon);
    };

    // Munch a token into the current key.
    (@object $object:ident ($($key:tt)*) ($tt:tt $($rest:tt)*) $copy:tt) => {
        $crate::xpc_internal!(@object $object ($($key)* $tt) ($($rest)*) ($($rest)*));
    };

    //////////////////////////////////////////////////////////////////////////
    // The main implementation.
    //////////////////////////////////////////////////////////////////////////

    (null) => {
        // XPC does not have a native null type. We'll use an empty string as a convention.
        $crate::xpc::XPCObject::String("".to_string())
    };

    (true) => {
        $crate::xpc::XPCObject::Bool(true)
    };

    (false) => {
        $crate::xpc::XPCObject::Bool(false)
    };

    ([ $($tt:tt)+ ]) => {
        $crate::xpc::XPCObject::Array($crate::xpc_internal!(@array [] $($tt)+))
    };

    ({ $($tt:tt)+ }) => {
        $crate::xpc::XPCObject::Dictionary({
            let mut object = $crate::xpc::Dictionary::new();
            $crate::xpc_internal!(@object object () ($($tt)+) ($($tt)+));
            object
        })
    };

    // Any other expression that implements XpcConvertible.
    // Must be below every other rule.
    ($other:expr) => {
        $crate::xpc::xpc_macro::xpc_to_value($other)
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! xpc_unexpected {
    () => {};
}

/// Conversion traits and helper functions for the `xpc!` macro.
use crate::xpc::{Dictionary, XPCObject};
use std::collections::{BTreeMap, HashMap};

// Convert various types to XPCObject
pub trait XpcConvertible {
    fn to_xpc_value(self) -> XPCObject;
}

#[doc(hidden)]
pub fn xpc_to_value<T: XpcConvertible>(value: T) -> XPCObject {
    value.to_xpc_value()
}

// Base implementation
impl XpcConvertible for XPCObject {
    fn to_xpc_value(self) -> XPCObject {
        self
    }
}

// Primitive implementations
impl XpcConvertible for String {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::String(self)
    }
}
impl XpcConvertible for &str {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::String(self.to_string())
    }
}
impl XpcConvertible for bool {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Bool(self)
    }
}

// Signed integer implementations
macro_rules! impl_xpc_convertible_for_signed {
        ($($t:ty),*) => {
            $(impl XpcConvertible for $t {
                fn to_xpc_value(self) -> XPCObject { XPCObject::Int64(self as i64) }
            })*
        };
    }
impl_xpc_convertible_for_signed!(i8, i16, i32, i64, isize);

// Unsigned integer implementations
macro_rules! impl_xpc_convertible_for_unsigned {
        ($($t:ty),*) => {
            $(impl XpcConvertible for $t {
                fn to_xpc_value(self) -> XPCObject { XPCObject::UInt64(self as u64) }
            })*
        };
    }
impl_xpc_convertible_for_unsigned!(u16, u32, u64, usize);

// Floating point implementations
impl XpcConvertible for f32 {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Double(self as f64)
    }
}
impl XpcConvertible for f64 {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Double(self)
    }
}

// Other XPC-specific types
impl XpcConvertible for Vec<u8> {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Data(self)
    }
}
impl XpcConvertible for &[u8] {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Data(self.to_vec())
    }
}
impl XpcConvertible for uuid::Uuid {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Uuid(self)
    }
}
impl XpcConvertible for std::time::SystemTime {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Date(self)
    }
}

// Collection implementations
impl<T: XpcConvertible> XpcConvertible for Vec<T> {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Array(self.into_iter().map(XpcConvertible::to_xpc_value).collect())
    }
}
impl<T: XpcConvertible + Clone> XpcConvertible for &[T] {
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Array(
            self.iter()
                .cloned()
                .map(XpcConvertible::to_xpc_value)
                .collect(),
        )
    }
}
impl<K, V> XpcConvertible for HashMap<K, V>
where
    K: Into<String>,
    V: XpcConvertible,
{
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Dictionary(
            self.into_iter()
                .map(|(k, v)| (k.into(), v.to_xpc_value()))
                .collect(),
        )
    }
}
impl<K, V> XpcConvertible for BTreeMap<K, V>
where
    K: Into<String>,
    V: XpcConvertible,
{
    fn to_xpc_value(self) -> XPCObject {
        XPCObject::Dictionary(
            self.into_iter()
                .map(|(k, v)| (k.into(), v.to_xpc_value()))
                .collect(),
        )
    }
}

// Optional value handling (for `key:? value`)
pub trait MaybeXpc {
    fn into_option_xpc(self) -> Option<XPCObject>;
}
impl<T: XpcConvertible> MaybeXpc for T {
    fn into_option_xpc(self) -> Option<XPCObject> {
        Some(self.to_xpc_value())
    }
}
impl<T: XpcConvertible> MaybeXpc for Option<T> {
    fn into_option_xpc(self) -> Option<XPCObject> {
        self.map(XpcConvertible::to_xpc_value)
    }
}

#[doc(hidden)]
pub fn xpc_maybe<T: MaybeXpc>(v: T) -> Option<XPCObject> {
    v.into_option_xpc()
}

// Dictionary merging (for `:< dict`)
pub trait IntoXpcDict {
    fn into_xpc_dict(self) -> Dictionary;
}
impl IntoXpcDict for Dictionary {
    fn into_xpc_dict(self) -> Dictionary {
        self
    }
}
impl IntoXpcDict for XPCObject {
    fn into_xpc_dict(self) -> Dictionary {
        match self {
            XPCObject::Dictionary(d) => d,
            other => panic!(
                "xpc! macro merge `:<` expects a dictionary, found {:?}",
                other
            ),
        }
    }
}
impl<K, V> IntoXpcDict for HashMap<K, V>
where
    K: Into<String>,
    V: XpcConvertible,
{
    fn into_xpc_dict(self) -> Dictionary {
        self.into_iter()
            .map(|(k, v)| (k.into(), v.to_xpc_value()))
            .collect()
    }
}

// Optional dictionary merging (for `:<? dict`)
pub trait MaybeIntoXpcDict {
    fn into_option_xpc_dict(self) -> Option<Dictionary>;
}
impl<T: IntoXpcDict> MaybeIntoXpcDict for T {
    fn into_option_xpc_dict(self) -> Option<Dictionary> {
        Some(self.into_xpc_dict())
    }
}
impl<T: IntoXpcDict> MaybeIntoXpcDict for Option<T> {
    fn into_option_xpc_dict(self) -> Option<Dictionary> {
        self.map(IntoXpcDict::into_xpc_dict)
    }
}

#[doc(hidden)]
pub fn maybe_into_xpc_dict<T: MaybeIntoXpcDict>(v: T) -> Option<Dictionary> {
    v.into_option_xpc_dict()
}

#[cfg(test)]
mod tests {
    use crate::xpc::{Dictionary, XPCObject};
    use uuid::Uuid;

    #[test]
    fn test_xpc_macro_primitives() {
        assert_eq!(xpc!(null), XPCObject::String("".to_string()));
        assert_eq!(xpc!(true), XPCObject::Bool(true));
        assert_eq!(xpc!(-123), XPCObject::Int64(-123));
        assert_eq!(xpc!(123), XPCObject::Int64(123));
        assert_eq!(xpc!(123u32), XPCObject::UInt64(123));
        assert_eq!(xpc!(123u64), XPCObject::UInt64(123));
        assert_eq!(xpc!(123.45), XPCObject::Double(123.45));
        assert_eq!(xpc!("hello"), XPCObject::String("hello".to_string()));
    }

    #[test]
    fn test_xpc_macro_collections() {
        let arr = xpc!([1, "two", true]);
        match arr {
            XPCObject::Array(vec) => {
                assert_eq!(vec.len(), 3);
                assert_eq!(vec[0], XPCObject::Int64(1));
                assert_eq!(vec[1], XPCObject::String("two".to_string()));
                assert_eq!(vec[2], XPCObject::Bool(true));
            }
            _ => panic!("Expected array"),
        }

        let dict = xpc!({
            "key1": 1u64,
            "key2": "value2"
        });
        match dict {
            XPCObject::Dictionary(map) => {
                assert_eq!(map.len(), 2);
                assert_eq!(map.get("key1"), Some(&XPCObject::UInt64(1)));
                assert_eq!(
                    map.get("key2"),
                    Some(&XPCObject::String("value2".to_string()))
                );
            }
            _ => panic!("Expected dictionary"),
        }
    }

    #[test]
    fn test_xpc_macro_interpolation_and_optional() {
        let my_uuid = Uuid::new_v4();
        let optional_field: Option<String> = None;
        let present_field = Some("I'm here");
        let base = xpc!({ "base_field": true });
        let optional_base: Option<Dictionary> = None;

        let obj = xpc!({
            "id": my_uuid,
            "optional_field":? optional_field,
            "present_field":? present_field,
            :< base,
            :<? optional_base,
            "arr": [?Some(1), ?None::<i32>, 3],
        });

        if let XPCObject::Dictionary(dict) = obj {
            assert_eq!(dict.get("id"), Some(&XPCObject::Uuid(my_uuid)));
            assert!(dict.get("optional_field").is_none());
            assert_eq!(
                dict.get("present_field"),
                Some(&XPCObject::String("I'm here".to_string()))
            );
            assert_eq!(dict.get("base_field"), Some(&XPCObject::Bool(true)));

            if let Some(XPCObject::Array(arr)) = dict.get("arr") {
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0], XPCObject::Int64(1));
                assert_eq!(arr[1], XPCObject::Int64(3));
            } else {
                panic!("Expected array for 'arr' key");
            }
        } else {
            panic!("Expected dictionary");
        }
    }
}
