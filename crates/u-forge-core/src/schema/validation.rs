//! Pure property normalization and validation.
//!
//! Cache lookup and persistence stay in [`super::SchemaManager`]. This module
//! owns the one recursive interpretation of [`PropertyType`] and
//! [`ValidationRule`] used by strict mutations, preflight coercion, and import.

use super::{ObjectTypeSchema, PropertySchema, PropertyType, ValidationRule};
use serde_json::{Map, Number, Value};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoercionPolicy {
    None,
    PrimitiveStrings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownPropertyPolicy {
    Error,
    Warning,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NormalizationPolicy {
    pub coercion: CoercionPolicy,
    pub unknown_properties: UnknownPropertyPolicy,
}

impl NormalizationPolicy {
    pub(crate) const STRICT: Self = Self {
        coercion: CoercionPolicy::None,
        unknown_properties: UnknownPropertyPolicy::Error,
    };

    pub(crate) const PREFLIGHT: Self = Self {
        coercion: CoercionPolicy::PrimitiveStrings,
        unknown_properties: UnknownPropertyPolicy::Error,
    };

    pub(crate) const WARNING_ONLY_UNKNOWN: Self = Self {
        coercion: CoercionPolicy::None,
        unknown_properties: UnknownPropertyPolicy::Warning,
    };

    pub(crate) const IMPORT: Self = Self {
        coercion: CoercionPolicy::PrimitiveStrings,
        unknown_properties: UnknownPropertyPolicy::Drop,
    };
}

#[derive(Debug, Clone)]
pub(crate) struct PropertyNormalization {
    pub normalized: Value,
    pub errors: Vec<PropertyIssue>,
    pub warnings: Vec<PropertyIssue>,
}

impl PropertyNormalization {
    fn valid(normalized: Value) -> Self {
        Self {
            normalized,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn error(original: &Value, issue: PropertyIssue) -> Self {
        Self {
            normalized: original.clone(),
            errors: vec![issue],
            warnings: Vec::new(),
        }
    }

    fn append(&mut self, mut other: Self) {
        self.errors.append(&mut other.errors);
        self.warnings.append(&mut other.warnings);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ObjectNormalization {
    pub normalized: Map<String, Value>,
    pub errors: Vec<PropertyIssue>,
    pub warnings: Vec<PropertyIssue>,
}

pub(crate) fn normalize_object_properties(
    schema: &ObjectTypeSchema,
    properties: &Map<String, Value>,
    policy: NormalizationPolicy,
) -> ObjectNormalization {
    normalize_object_map(
        &schema.properties,
        &schema.required_properties,
        properties,
        policy,
        "",
        true,
    )
}

pub(crate) fn normalize_property_value(
    path: &str,
    value: &Value,
    schema: &PropertySchema,
    policy: NormalizationPolicy,
) -> PropertyNormalization {
    if value.is_null() {
        return PropertyNormalization::error(
            value,
            PropertyIssue::TypeMismatch {
                key: path.to_string(),
                expected: schema.property_type.name().to_string(),
                actual: "null".to_string(),
            },
        );
    }

    let mut result = match &schema.property_type {
        PropertyType::String | PropertyType::Text | PropertyType::Reference(_) => {
            if value.is_string() {
                PropertyNormalization::valid(value.clone())
            } else {
                type_mismatch(path, value, &schema.property_type)
            }
        }
        PropertyType::Number => match value {
            Value::Number(_) => PropertyNormalization::valid(value.clone()),
            Value::String(string) if policy.coercion == CoercionPolicy::PrimitiveStrings => string
                .parse::<f64>()
                .ok()
                .and_then(Number::from_f64)
                .map_or_else(
                    || type_mismatch(path, value, &schema.property_type),
                    |number| PropertyNormalization::valid(Value::Number(number)),
                ),
            _ => type_mismatch(path, value, &schema.property_type),
        },
        PropertyType::Boolean => match value {
            Value::Bool(_) => PropertyNormalization::valid(value.clone()),
            Value::String(string) if policy.coercion == CoercionPolicy::PrimitiveStrings => {
                match string.to_lowercase().as_str() {
                    "true" | "yes" | "1" => PropertyNormalization::valid(Value::Bool(true)),
                    "false" | "no" | "0" => PropertyNormalization::valid(Value::Bool(false)),
                    _ => type_mismatch(path, value, &schema.property_type),
                }
            }
            _ => type_mismatch(path, value, &schema.property_type),
        },
        PropertyType::Enum(allowed) => match value {
            Value::String(string) if allowed.contains(string) => {
                PropertyNormalization::valid(value.clone())
            }
            Value::String(string) => PropertyNormalization::error(
                value,
                PropertyIssue::InvalidEnum {
                    key: path.to_string(),
                    value: string.clone(),
                    allowed: allowed.clone(),
                },
            ),
            _ => type_mismatch(path, value, &schema.property_type),
        },
        PropertyType::Array(element_type) => match value {
            Value::Array(elements) => {
                let mut normalized = elements.clone();
                let mut result = PropertyNormalization::valid(Value::Null);
                let element_schema =
                    PropertySchema::new((**element_type).clone(), "Array element".to_string());
                for (index, element) in elements.iter().enumerate() {
                    let element_result = normalize_property_value(
                        &format!("{path}[{index}]"),
                        element,
                        &element_schema,
                        policy,
                    );
                    normalized[index] = element_result.normalized.clone();
                    result.append(element_result);
                }
                result.normalized = Value::Array(normalized);
                result
            }
            _ => type_mismatch(path, value, &schema.property_type),
        },
        PropertyType::Object(nested_schema) => match value {
            Value::Object(object) => {
                let required = nested_schema
                    .iter()
                    .filter_map(|(name, schema)| {
                        schema
                            .validation
                            .as_ref()
                            .is_some_and(|validation| validation.required)
                            .then_some(name.clone())
                    })
                    .collect::<Vec<_>>();
                let nested =
                    normalize_object_map(nested_schema, &required, object, policy, path, false);
                PropertyNormalization {
                    normalized: Value::Object(nested.normalized),
                    errors: nested.errors,
                    warnings: nested.warnings,
                }
            }
            _ => type_mismatch(path, value, &schema.property_type),
        },
    };

    if result.errors.is_empty()
        && let Some(validation) = &schema.validation
    {
        apply_validation_rules(path, &result.normalized, validation, &mut result.errors);
    }
    result
}

fn normalize_object_map(
    schema: &std::collections::HashMap<String, PropertySchema>,
    required_properties: &[String],
    properties: &Map<String, Value>,
    policy: NormalizationPolicy,
    parent_path: &str,
    top_level: bool,
) -> ObjectNormalization {
    let mut normalized = properties.clone();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let mut required = required_properties.to_vec();
    required.sort();
    required.dedup();
    for name in required {
        if top_level && name == "name" {
            continue;
        }
        let path = join_path(parent_path, &name);
        if properties.get(&name).is_none_or(|value| value.is_null()) {
            errors.push(PropertyIssue::MissingRequired { key: path });
        }
    }

    let mut keys = properties.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    for key in keys {
        let value = properties
            .get(&key)
            .expect("key was collected from this property map");
        let path = join_path(parent_path, &key);
        let Some(property_schema) = schema.get(&key) else {
            let issue = PropertyIssue::UnknownProperty { key: path };
            match policy.unknown_properties {
                UnknownPropertyPolicy::Error => errors.push(issue),
                UnknownPropertyPolicy::Warning | UnknownPropertyPolicy::Drop => {
                    warnings.push(issue)
                }
            }
            if policy.unknown_properties == UnknownPropertyPolicy::Drop {
                normalized.remove(&key);
            }
            continue;
        };

        // A required null has already produced the more actionable missing
        // diagnostic; avoid a duplicate type mismatch for the same value.
        if value.is_null() && required_properties.contains(&key) {
            continue;
        }

        let value_result = normalize_property_value(&path, value, property_schema, policy);
        normalized.insert(key, value_result.normalized);
        errors.extend(value_result.errors);
        warnings.extend(value_result.warnings);
    }

    ObjectNormalization {
        normalized,
        errors,
        warnings,
    }
}

fn type_mismatch(path: &str, value: &Value, expected: &PropertyType) -> PropertyNormalization {
    PropertyNormalization::error(
        value,
        PropertyIssue::TypeMismatch {
            key: path.to_string(),
            expected: expected.name().to_string(),
            actual: value_kind(value).to_string(),
        },
    )
}

fn apply_validation_rules(
    path: &str,
    value: &Value,
    validation: &ValidationRule,
    issues: &mut Vec<PropertyIssue>,
) {
    if let Value::String(string) = value {
        if let Some(minimum) = validation.min_length
            && string.len() < minimum
        {
            issues.push(PropertyIssue::ValidationFailed {
                key: path.to_string(),
                message: format!("minimum length is {minimum}"),
            });
        }
        if let Some(maximum) = validation.max_length
            && string.len() > maximum
        {
            issues.push(PropertyIssue::ValidationFailed {
                key: path.to_string(),
                message: format!("maximum length is {maximum}"),
            });
        }
        if let Some(pattern) = &validation.pattern {
            match regex::Regex::new(pattern) {
                Ok(regex) if !regex.is_match(string) => {
                    issues.push(PropertyIssue::ValidationFailed {
                        key: path.to_string(),
                        message: format!("must match pattern {pattern}"),
                    });
                }
                Err(_) => issues.push(PropertyIssue::ValidationFailed {
                    key: path.to_string(),
                    message: format!("schema contains invalid regex pattern {pattern}"),
                }),
                Ok(_) => {}
            }
        }
        if let Some(allowed) = &validation.allowed_values
            && !allowed.contains(string)
        {
            issues.push(PropertyIssue::InvalidEnum {
                key: path.to_string(),
                value: string.clone(),
                allowed: allowed.clone(),
            });
        }
    }

    if let Value::Number(number) = value
        && let Some(number) = number.as_f64()
    {
        if let Some(minimum) = validation.min_value
            && number < minimum
        {
            issues.push(PropertyIssue::ValidationFailed {
                key: path.to_string(),
                message: format!("minimum value is {minimum}"),
            });
        }
        if let Some(maximum) = validation.max_value
            && number > maximum
        {
            issues.push(PropertyIssue::ValidationFailed {
                key: path.to_string(),
                message: format!("maximum value is {maximum}"),
            });
        }
    }
}

fn join_path(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Describes one validation or coercion issue at a stable property path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyIssue {
    /// Object type is absent from every authoritative cached schema.
    UnknownObjectType {
        object_type: String,
        valid: Vec<String>,
    },
    /// A required property is absent or explicitly null.
    MissingRequired { key: String },
    /// Value type does not match schema and could not be automatically coerced.
    TypeMismatch {
        key: String,
        expected: String,
        actual: String,
    },
    /// Property key is not declared in the schema for this object type.
    UnknownProperty { key: String },
    /// String value is not in the enum or allowed-values list.
    InvalidEnum {
        key: String,
        value: String,
        allowed: Vec<String>,
    },
    /// A length, numeric range, or regex constraint failed.
    ValidationFailed { key: String, message: String },
}

impl PropertyIssue {
    pub(crate) fn key(&self) -> &str {
        match self {
            Self::UnknownObjectType { .. } => "object_type",
            Self::MissingRequired { key }
            | Self::TypeMismatch { key, .. }
            | Self::UnknownProperty { key }
            | Self::InvalidEnum { key, .. }
            | Self::ValidationFailed { key, .. } => key,
        }
    }
}

impl fmt::Display for PropertyIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownObjectType { object_type, valid } => write!(
                f,
                "unknown object type '{object_type}' (valid types: {})",
                valid.join(", ")
            ),
            Self::MissingRequired { key } => {
                write!(f, "required property '{key}' is missing")
            }
            Self::TypeMismatch {
                key,
                expected,
                actual,
            } => write!(f, "property '{key}': expected {expected}, got {actual}"),
            Self::UnknownProperty { key } => {
                write!(f, "property '{key}' is not declared in schema")
            }
            Self::InvalidEnum {
                key,
                value,
                allowed,
            } => write!(
                f,
                "property '{key}': '{value}' is not an allowed value (allowed: {})",
                allowed.join(", ")
            ),
            Self::ValidationFailed { key, message } => {
                write!(f, "property '{key}': {message}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ValidationRule;
    use serde_json::json;
    use std::collections::HashMap;

    fn comprehensive_schema() -> ObjectTypeSchema {
        let nested = HashMap::from([
            (
                "alias".to_string(),
                PropertySchema::string("alias").with_validation(ValidationRule::required()),
            ),
            ("count".to_string(), PropertySchema::number("count")),
        ]);
        ObjectTypeSchema::new("record".to_string(), "record".to_string())
            .with_property("title".to_string(), PropertySchema::string("title"))
            .with_property("notes".to_string(), PropertySchema::text("notes"))
            .with_property("rating".to_string(), PropertySchema::number("rating"))
            .with_property("active".to_string(), PropertySchema::boolean("active"))
            .with_property(
                "scores".to_string(),
                PropertySchema::array(PropertyType::Number),
            )
            .with_property(
                "profile".to_string(),
                PropertySchema::new(PropertyType::Object(nested), "profile".to_string()),
            )
            .with_property("owner".to_string(), PropertySchema::reference("npc"))
            .with_property(
                "state".to_string(),
                PropertySchema::new(
                    PropertyType::Enum(vec!["draft".to_string(), "final".to_string()]),
                    "state".to_string(),
                ),
            )
    }

    #[test]
    fn preflight_normalizes_every_property_type_recursively() {
        let schema = comprehensive_schema();
        let properties = json!({
            "title": "Record",
            "notes": "Long form",
            "rating": "3.5",
            "active": "YES",
            "scores": ["1", 2],
            "profile": { "alias": "R", "count": "4" },
            "owner": "npc-id",
            "state": "draft"
        })
        .as_object()
        .unwrap()
        .clone();

        let result =
            normalize_object_properties(&schema, &properties, NormalizationPolicy::PREFLIGHT);

        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.normalized["rating"], json!(3.5));
        assert_eq!(result.normalized["active"], json!(true));
        assert_eq!(result.normalized["scores"], json!([1.0, 2]));
        assert_eq!(result.normalized["profile"]["count"], json!(4.0));
    }

    #[test]
    fn strict_policy_rejects_otherwise_coercible_values() {
        let schema = comprehensive_schema();
        let properties = json!({ "active": "yes", "rating": "3" })
            .as_object()
            .unwrap()
            .clone();

        let result = normalize_object_properties(&schema, &properties, NormalizationPolicy::STRICT);
        assert_eq!(
            result.errors,
            vec![
                PropertyIssue::TypeMismatch {
                    key: "active".to_string(),
                    expected: "boolean".to_string(),
                    actual: "string".to_string(),
                },
                PropertyIssue::TypeMismatch {
                    key: "rating".to_string(),
                    expected: "number".to_string(),
                    actual: "string".to_string(),
                },
            ]
        );
    }

    #[test]
    fn rules_and_multiple_issues_have_deterministic_order() {
        let schema = ObjectTypeSchema::new("record".to_string(), "record".to_string())
            .with_property(
                "summary".to_string(),
                PropertySchema::string("summary").with_validation(
                    ValidationRule::new()
                        .with_length_range(Some(3), Some(8))
                        .with_pattern("^[A-Z]".to_string())
                        .with_allowed_values(vec!["Alpha".to_string(), "Beta".to_string()]),
                ),
            )
            .with_property(
                "rating".to_string(),
                PropertySchema::number("rating")
                    .with_validation(ValidationRule::new().with_value_range(Some(1.0), Some(5.0))),
            );
        let properties = json!({ "summary": "x", "rating": 9 })
            .as_object()
            .unwrap()
            .clone();

        let first =
            normalize_object_properties(&schema, &properties, NormalizationPolicy::PREFLIGHT);
        let second =
            normalize_object_properties(&schema, &properties, NormalizationPolicy::PREFLIGHT);
        assert_eq!(first.errors, second.errors);
        assert_eq!(
            first
                .errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec![
                "property 'rating': maximum value is 5",
                "property 'summary': minimum length is 3",
                "property 'summary': must match pattern ^[A-Z]",
                "property 'summary': 'x' is not an allowed value (allowed: Alpha, Beta)",
            ]
        );
    }

    #[test]
    fn absent_and_null_required_fields_report_once_in_sorted_order() {
        let schema = ObjectTypeSchema::new("record".to_string(), "record".to_string())
            .with_property("alpha".to_string(), PropertySchema::string("alpha"))
            .with_property("beta".to_string(), PropertySchema::string("beta"))
            .with_required_property("beta".to_string())
            .with_required_property("alpha".to_string());
        let properties = json!({ "beta": null }).as_object().unwrap().clone();

        let result =
            normalize_object_properties(&schema, &properties, NormalizationPolicy::PREFLIGHT);
        assert_eq!(
            result.errors,
            vec![
                PropertyIssue::MissingRequired {
                    key: "alpha".to_string(),
                },
                PropertyIssue::MissingRequired {
                    key: "beta".to_string(),
                },
            ]
        );
    }

    #[test]
    fn unknown_property_policy_selects_error_warning_or_drop() {
        let schema = ObjectTypeSchema::new("record".to_string(), "record".to_string());
        let properties = json!({ "unknown": 1 }).as_object().unwrap().clone();

        let strict = normalize_object_properties(&schema, &properties, NormalizationPolicy::STRICT);
        assert_eq!(strict.errors.len(), 1);
        assert!(strict.normalized.contains_key("unknown"));

        let warning = normalize_object_properties(
            &schema,
            &properties,
            NormalizationPolicy::WARNING_ONLY_UNKNOWN,
        );
        assert_eq!(warning.warnings.len(), 1);
        assert!(warning.normalized.contains_key("unknown"));

        let imported =
            normalize_object_properties(&schema, &properties, NormalizationPolicy::IMPORT);
        assert_eq!(imported.warnings.len(), 1);
        assert!(!imported.normalized.contains_key("unknown"));
    }

    #[test]
    fn failed_primitive_coercions_remain_type_errors() {
        let schema = ObjectTypeSchema::new("record".to_string(), "record".to_string())
            .with_property("active".to_string(), PropertySchema::boolean("active"))
            .with_property("rating".to_string(), PropertySchema::number("rating"));
        let properties = json!({ "active": "sometimes", "rating": "NaN" })
            .as_object()
            .unwrap()
            .clone();

        let result =
            normalize_object_properties(&schema, &properties, NormalizationPolicy::PREFLIGHT);
        assert_eq!(result.errors.len(), 2);
        assert!(
            result
                .errors
                .iter()
                .all(|issue| matches!(issue, PropertyIssue::TypeMismatch { .. }))
        );
    }
}
