#[macro_export]
macro_rules! impl_to_structs {
    (
        $( $name:ident $(<$li:lifetime>)? ),+;
        $body:tt
    ) => {
        $(
            impl $name $(<$li>)? $body
        )+
    };
}

#[macro_export]
macro_rules! impl_trait_to_structs {
    (
        $trit:ident for $( $name:ident $(<$li:lifetime>)? ),+;
        $body:tt
    ) => {
        $(
            impl $trit for $name $(<$li>)? $body
        )+
    };
}
