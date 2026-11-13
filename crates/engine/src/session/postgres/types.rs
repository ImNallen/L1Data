use crate::model::{SqlTypeRef, TypeCategory};
use tokio_postgres::types::{Kind, Type};

pub fn type_ref(t: &Type) -> SqlTypeRef {
    SqlTypeRef { display: type_display(t), category: type_category(t) }
}

fn type_display(t: &Type) -> String {
    match t.kind() {
        Kind::Array(elem) => format!("{}[]", type_display(elem)),
        _ if t.schema() == "pg_catalog" => builtin_name(t.name()).to_owned(),
        _ if t.schema() == "public" => t.name().to_owned(),
        _ => format!("{}.{}", t.schema(), t.name()),
    }
}

fn builtin_name(internal: &str) -> &str {
    match internal {
        "int8" => "bigint",
        "int4" => "integer",
        "int2" => "smallint",
        "float4" => "real",
        "float8" => "double precision",
        "bool" => "boolean",
        "bpchar" => "char",
        other => other,
    }
}

/// The category of a type the catalog names by OID, with `typtype` and `typcategory` from `pg_type`.
/// The catalog resolves a domain to its base type first. User types carry no element or member
/// types here because the category never needs them.
pub fn catalog_category(oid: u32, typtype: i8, typcategory: i8) -> TypeCategory {
    let kind = match (typtype as u8, typcategory as u8) {
        (b'e', _) => Kind::Enum(Vec::new()),
        (b'c', _) => Kind::Composite(Vec::new()),
        (b'r', _) => Kind::Range(Type::UNKNOWN),
        (b'm', _) => Kind::Multirange(Type::UNKNOWN),
        (_, b'A') => Kind::Array(Type::UNKNOWN),
        _ => return Type::from_oid(oid).map_or(TypeCategory::Other, |t| type_category(&t)),
    };
    type_category(&Type::new(String::new(), oid, kind, String::new()))
}

fn type_category(t: &Type) -> TypeCategory {
    match t.kind() {
        Kind::Array(_) => TypeCategory::Array,
        Kind::Enum(_) => TypeCategory::Enum,
        Kind::Composite(_) => TypeCategory::Composite,
        Kind::Range(_) | Kind::Multirange(_) => TypeCategory::Range,
        Kind::Domain(inner) => type_category(inner),
        Kind::Simple | Kind::Pseudo => match *t {
            Type::INT2 | Type::INT4 | Type::INT8 | Type::OID => TypeCategory::Integer,
            Type::FLOAT4 | Type::FLOAT8 | Type::NUMERIC | Type::MONEY => TypeCategory::Numeric,
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::CHAR | Type::XML => TypeCategory::Text,
            Type::BOOL => TypeCategory::Bool,
            Type::DATE | Type::TIME | Type::TIMETZ | Type::TIMESTAMP | Type::TIMESTAMPTZ | Type::INTERVAL => {
                TypeCategory::Temporal
            }
            Type::JSON | Type::JSONB => TypeCategory::Json,
            Type::BYTEA => TypeCategory::Binary,
            Type::UUID => TypeCategory::Uuid,
            Type::INET | Type::CIDR | Type::MACADDR | Type::MACADDR8 => TypeCategory::Network,
            _ => TypeCategory::Other,
        },
        _ => TypeCategory::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtin_and_array_names() {
        let r = type_ref(&Type::INT8);
        assert_eq!((r.display.as_str(), r.category), ("bigint", TypeCategory::Integer));
        assert_eq!(type_ref(&Type::INT4_ARRAY).display, "integer[]");
        assert_eq!(type_ref(&Type::INT4_ARRAY).category, TypeCategory::Array);
        assert_eq!(type_ref(&Type::TIMESTAMPTZ).display, "timestamptz");
        assert_eq!(type_ref(&Type::TIMESTAMPTZ).category, TypeCategory::Temporal);
        assert_eq!(type_ref(&Type::TEXT).category, TypeCategory::Text);
        assert_eq!(type_ref(&Type::BPCHAR).display, "char");
        assert_eq!(type_ref(&Type::NUMERIC).category, TypeCategory::Numeric);
        assert_eq!(type_ref(&Type::OID).category, TypeCategory::Integer);
        assert_eq!(type_ref(&Type::FLOAT8).category, TypeCategory::Numeric);
        assert_eq!(type_ref(&Type::JSONB).category, TypeCategory::Json);
        assert_eq!(type_ref(&Type::BYTEA).category, TypeCategory::Binary);
        assert_eq!(type_ref(&Type::UUID).category, TypeCategory::Uuid);
        assert_eq!(type_ref(&Type::INET).category, TypeCategory::Network);
        assert_eq!(type_ref(&Type::INT4_RANGE).category, TypeCategory::Range);
        assert_eq!(type_ref(&Type::POINT).category, TypeCategory::Other);
        assert_eq!(type_ref(&Type::BOOL).display, "boolean");
    }

    #[test]
    fn catalog_types_share_the_result_categories() {
        let (base, enumerated, composite, range, array) = (b'b' as i8, b'e' as i8, b'c' as i8, b'r' as i8, b'A' as i8);
        assert_eq!(catalog_category(20, base, b'N' as i8), TypeCategory::Integer);
        assert_eq!(catalog_category(1700, base, b'N' as i8), TypeCategory::Numeric);
        assert_eq!(catalog_category(25, base, b'S' as i8), TypeCategory::Text);
        assert_eq!(catalog_category(70000, enumerated, b'E' as i8), TypeCategory::Enum);
        assert_eq!(catalog_category(70001, composite, b'C' as i8), TypeCategory::Composite);
        assert_eq!(catalog_category(70002, range, b'R' as i8), TypeCategory::Range);
        assert_eq!(catalog_category(70003, base, array), TypeCategory::Array);
        assert_eq!(catalog_category(70004, base, b'U' as i8), TypeCategory::Other);
    }

    #[test]
    fn user_types_show_schema_outside_public() {
        let public_enum = Type::new("mood".into(), 70000, Kind::Enum(vec![]), "public".into());
        assert_eq!(type_ref(&public_enum).display, "mood");
        assert_eq!(type_ref(&public_enum).category, TypeCategory::Enum);
        let lab_enum = Type::new("mood".into(), 70001, Kind::Enum(vec![]), "lab".into());
        assert_eq!(type_ref(&lab_enum).display, "lab.mood");
        let arr = Type::new("_mood".into(), 70002, Kind::Array(lab_enum), "lab".into());
        assert_eq!(type_ref(&arr).display, "lab.mood[]");
        let domain = Type::new("posint".into(), 70003, Kind::Domain(Type::INT4), "public".into());
        assert_eq!(type_ref(&domain).category, TypeCategory::Integer);
    }
}
