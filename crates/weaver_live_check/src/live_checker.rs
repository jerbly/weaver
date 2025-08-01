// SPDX-License-Identifier: Apache-2.0

//! Holds the registry, helper structs, and the advisors for the live check

use serde::Serialize;
use std::collections::HashMap;
use std::rc::Rc;
use weaver_semconv::{attribute::AttributeType, group::GroupType};

use weaver_forge::registry::{ResolvedGroup, ResolvedRegistry};
use weaver_resolved_schema::attribute::Attribute;

use crate::advice::Advisor;

/// Holds the registry, helper structs, and the advisors for the live check
#[derive(Serialize)]
pub struct LiveChecker {
    /// The resolved registry
    pub registry: ResolvedRegistry,
    semconv_attributes: HashMap<String, Rc<Attribute>>,
    semconv_templates: HashMap<String, Rc<Attribute>>,
    semconv_metrics: HashMap<String, Rc<ResolvedGroup>>,
    /// The advisors to run
    #[serde(skip)]
    pub advisors: Vec<Box<dyn Advisor>>,
    #[serde(skip)]
    templates_by_length: Vec<(String, Rc<Attribute>)>,
}

impl LiveChecker {
    #[must_use]
    /// Create a new LiveChecker
    pub fn new(registry: ResolvedRegistry, advisors: Vec<Box<dyn Advisor>>) -> Self {
        // Create a hashmap of attributes for quick lookup
        let mut semconv_attributes = HashMap::new();
        let mut semconv_templates = HashMap::new();
        let mut templates_by_length = Vec::new();
        // Hashmap of metrics by name
        let mut semconv_metrics = HashMap::new();

        for group in &registry.groups {
            if group.r#type == GroupType::Metric {
                if let Some(metric_name) = &group.metric_name {
                    let group_rc = Rc::new(group.clone());
                    let _ = semconv_metrics.insert(metric_name.clone(), group_rc);
                }
            }
            for attribute in &group.attributes {
                let attribute_rc = Rc::new(attribute.clone());
                match attribute.r#type {
                    AttributeType::Template(_) => {
                        templates_by_length.push((attribute.name.clone(), attribute_rc.clone()));
                        let _ = semconv_templates.insert(attribute.name.clone(), attribute_rc);
                    }
                    _ => {
                        let _ = semconv_attributes.insert(attribute.name.clone(), attribute_rc);
                    }
                }
            }
        }

        // Sort templates by name length in descending order
        templates_by_length.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()));

        LiveChecker {
            registry,
            semconv_attributes,
            semconv_templates,
            semconv_metrics,
            advisors,
            templates_by_length,
        }
    }

    /// Add an advisor
    pub fn add_advisor(&mut self, advisor: Box<dyn Advisor>) {
        self.advisors.push(advisor);
    }

    /// Find an attribute in the registry
    #[must_use]
    pub fn find_attribute(&self, name: &str) -> Option<Rc<Attribute>> {
        self.semconv_attributes.get(name).map(Rc::clone)
    }

    /// Find a metric in the registry
    #[must_use]
    pub fn find_metric(&self, name: &str) -> Option<Rc<ResolvedGroup>> {
        self.semconv_metrics.get(name).map(Rc::clone)
    }

    /// Find a template in the registry
    #[must_use]
    pub fn find_template(&self, attribute_name: &str) -> Option<Rc<Attribute>> {
        // Use the pre-sorted list to find the first (longest) matching template
        for (template_name, attribute) in &self.templates_by_length {
            if attribute_name.starts_with(template_name) {
                return Some(Rc::clone(attribute));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use crate::{
        advice::{DeprecatedAdvisor, EnumAdvisor, RegoAdvisor, StabilityAdvisor, TypeAdvisor},
        sample_attribute::SampleAttribute,
        sample_metric::{
            DataPoints, SampleExemplar, SampleExponentialHistogramDataPoint, SampleInstrument,
            SampleMetric, SampleNumberDataPoint,
        },
        LiveCheckRunner, LiveCheckStatistics, Sample,
    };

    use super::*;
    use serde_json::{json, Value};
    use weaver_checker::violation::{Advice, AdviceLevel};
    use weaver_forge::registry::{ResolvedGroup, ResolvedRegistry};
    use weaver_resolved_schema::attribute::Attribute;
    use weaver_semconv::{
        attribute::{
            AttributeType, EnumEntriesSpec, Examples, PrimitiveOrArrayTypeSpec, RequirementLevel,
            TemplateTypeSpec, ValueSpec,
        },
        group::{GroupType, InstrumentSpec, SpanKindSpec},
        stability::Stability,
    };

    fn get_all_advice(sample: &mut Sample) -> &mut [Advice] {
        match sample {
            Sample::Attribute(sample_attribute) => sample_attribute
                .live_check_result
                .as_mut() // Change to as_mut() to get a mutable reference
                .map(|result| &mut result.all_advice)
                .map_or(&mut [], |v| v),
            _ => &mut [],
        }
    }

    #[test]
    fn test_attribute_live_checker() {
        let registry = make_registry();

        let mut samples = vec![
            Sample::Attribute(SampleAttribute::try_from("test.string=value").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("testString2").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("test.deprecated=42").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("aws.s3.bucket.name").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("test.enum=foo").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("test.enum=example_variant1").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("test.enum=42.42").unwrap()),
            Sample::Attribute(
                SampleAttribute::try_from("test.string.not.allowed=example_value").unwrap(),
            ),
            Sample::Attribute(SampleAttribute::try_from("test.extends=new_value").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("test.template.my.key=42").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("test.deprecated.allowed=42").unwrap()),
        ];

        let advisors: Vec<Box<dyn Advisor>> = vec![
            Box::new(DeprecatedAdvisor),
            Box::new(StabilityAdvisor),
            Box::new(TypeAdvisor),
            Box::new(EnumAdvisor),
        ];

        let mut live_checker = LiveChecker::new(registry, advisors);
        let rego_advisor =
            RegoAdvisor::new(&live_checker, &None, &None).expect("Failed to create Rego advisor");
        live_checker.add_advisor(Box::new(rego_advisor));

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            let result = sample.run_live_check(&mut live_checker, &mut stats, None);
            assert!(result.is_ok());
        }
        stats.finalize();

        let all_advice = get_all_advice(&mut samples[0]);
        assert!(all_advice.is_empty());

        let all_advice = get_all_advice(&mut samples[1]);
        assert_eq!(all_advice.len(), 3);
        // make a sort of the advice
        all_advice.sort_by(|a, b| a.advice_type.cmp(&b.advice_type));
        assert_eq!(all_advice[0].advice_type, "invalid_format");
        assert_eq!(all_advice[0].value, Value::String("testString2".to_owned()));
        assert_eq!(
            all_advice[0].message,
            "Does not match name formatting rules"
        );
        assert_eq!(all_advice[1].advice_type, "missing_attribute");
        assert_eq!(all_advice[1].value, Value::String("testString2".to_owned()));
        assert_eq!(all_advice[1].message, "Does not exist in the registry");
        assert_eq!(all_advice[2].advice_type, "missing_namespace");
        assert_eq!(all_advice[2].value, Value::String("testString2".to_owned()));
        assert_eq!(all_advice[2].message, "Does not have a namespace");

        let all_advice = get_all_advice(&mut samples[2]);
        assert_eq!(all_advice.len(), 3);
        assert_eq!(all_advice[0].advice_type, "deprecated");
        assert_eq!(
            all_advice[0].value,
            Value::String("uncategorized".to_owned())
        );
        assert_eq!(all_advice[0].message, "note");

        assert_eq!(all_advice[1].advice_type, "stability");
        assert_eq!(all_advice[1].value, Value::String("development".to_owned()));
        assert_eq!(all_advice[1].message, "Is not stable");

        assert_eq!(all_advice[2].advice_type, "type_mismatch");
        assert_eq!(all_advice[2].value, Value::String("int".to_owned()));
        assert_eq!(all_advice[2].message, "Type should be `string`");

        let all_advice = get_all_advice(&mut samples[3]);
        assert_eq!(all_advice.len(), 1);
        assert_eq!(all_advice[0].advice_type, "missing_attribute");
        assert_eq!(
            all_advice[0].value,
            Value::String("aws.s3.bucket.name".to_owned())
        );
        assert_eq!(all_advice[0].message, "Does not exist in the registry");

        let all_advice = get_all_advice(&mut samples[4]);
        assert_eq!(all_advice.len(), 1);
        assert_eq!(all_advice[0].advice_type, "undefined_enum_variant");
        assert_eq!(all_advice[0].value, Value::String("foo".to_owned()));
        assert_eq!(all_advice[0].message, "Is not a defined variant");

        let all_advice = get_all_advice(&mut samples[6]);
        assert_eq!(all_advice.len(), 1);
        assert_eq!(all_advice[0].advice_type, "type_mismatch");
        assert_eq!(all_advice[0].value, Value::String("double".to_owned()));
        assert_eq!(all_advice[0].message, "Type should be `string` or `int`");

        let all_advice = get_all_advice(&mut samples[7]);

        // Make a sort of the advice
        all_advice.sort_by(|a, b| a.advice_type.cmp(&b.advice_type));
        assert_eq!(all_advice.len(), 3);

        assert_eq!(all_advice[0].advice_type, "extends_namespace");
        assert_eq!(all_advice[0].value, Value::String("test".to_owned()));
        assert_eq!(all_advice[0].message, "Extends existing namespace");
        assert_eq!(all_advice[1].advice_type, "illegal_namespace");
        assert_eq!(all_advice[1].value, Value::String("test.string".to_owned()));
        assert_eq!(
            all_advice[1].message,
            "Namespace matches existing attribute"
        );
        assert_eq!(all_advice[2].advice_type, "missing_attribute");
        assert_eq!(
            all_advice[2].value,
            Value::String("test.string.not.allowed".to_owned())
        );
        assert_eq!(all_advice[2].message, "Does not exist in the registry");

        let all_advice = get_all_advice(&mut samples[8]);
        assert_eq!(all_advice.len(), 2);
        assert_eq!(all_advice[0].advice_type, "missing_attribute");
        assert_eq!(
            all_advice[0].value,
            Value::String("test.extends".to_owned())
        );
        assert_eq!(all_advice[0].message, "Does not exist in the registry");
        assert_eq!(all_advice[1].advice_type, "extends_namespace");
        assert_eq!(all_advice[1].value, Value::String("test".to_owned()));
        assert_eq!(all_advice[1].message, "Extends existing namespace");

        // test.template
        let all_advice = get_all_advice(&mut samples[9]);
        assert_eq!(all_advice.len(), 2);
        assert_eq!(all_advice[0].advice_type, "template_attribute");
        assert_eq!(
            all_advice[0].value,
            Value::String("test.template".to_owned())
        );
        assert_eq!(all_advice[0].message, "Is a template");
        assert_eq!(all_advice[1].advice_type, "type_mismatch");
        assert_eq!(all_advice[1].value, Value::String("int".to_owned()));
        assert_eq!(all_advice[1].message, "Type should be `string`");

        // test.deprecated.allowed
        // Should not get illegal_namespace for extending a deprecated attribute
        let all_advice = get_all_advice(&mut samples[10]);
        assert_eq!(all_advice.len(), 2);
        assert_eq!(all_advice[0].advice_type, "missing_attribute");
        assert_eq!(
            all_advice[0].value,
            Value::String("test.deprecated.allowed".to_owned())
        );
        assert_eq!(all_advice[0].message, "Does not exist in the registry");
        assert_eq!(all_advice[1].advice_type, "extends_namespace");
        assert_eq!(all_advice[1].value, Value::String("test".to_owned()));
        assert_eq!(all_advice[1].message, "Extends existing namespace");

        // Check statistics
        assert_eq!(stats.total_entities, 11);
        assert_eq!(stats.total_advisories, 18);
        assert_eq!(stats.advice_level_counts.len(), 3);
        assert_eq!(stats.advice_level_counts[&AdviceLevel::Violation], 11);
        assert_eq!(stats.advice_level_counts[&AdviceLevel::Information], 5);
        assert_eq!(stats.advice_level_counts[&AdviceLevel::Improvement], 2);
        assert_eq!(stats.highest_advice_level_counts.len(), 2);
        assert_eq!(
            stats.highest_advice_level_counts[&AdviceLevel::Violation],
            8
        );
        assert_eq!(
            stats.highest_advice_level_counts[&AdviceLevel::Information],
            1
        );
        assert_eq!(stats.no_advice_count, 2);
        assert_eq!(stats.seen_registry_attributes.len(), 3);
        assert_eq!(stats.seen_registry_attributes["test.enum"], 3);
        assert_eq!(stats.seen_non_registry_attributes.len(), 6);
        assert_eq!(stats.registry_coverage, 1.0);
    }

    fn make_registry() -> ResolvedRegistry {
        ResolvedRegistry {
            registry_url: "TEST".to_owned(),
            groups: vec![ResolvedGroup {
                id: "test.comprehensive.internal".to_owned(),
                r#type: GroupType::Span,
                brief: "".to_owned(),
                note: "".to_owned(),
                prefix: "".to_owned(),
                entity_associations: vec![],
                extends: None,
                stability: Some(Stability::Stable),
                deprecated: None,
                attributes: vec![
                    Attribute {
                        name: "test.string".to_owned(),
                        r#type: AttributeType::PrimitiveOrArray(PrimitiveOrArrayTypeSpec::String),
                        examples: Some(Examples::Strings(vec![
                            "value1".to_owned(),
                            "value2".to_owned(),
                        ])),
                        brief: "".to_owned(),
                        tag: None,
                        requirement_level: RequirementLevel::Recommended {
                            text: "".to_owned(),
                        },
                        sampling_relevant: None,
                        note: "".to_owned(),
                        stability: Some(Stability::Stable),
                        deprecated: None,
                        prefix: false,
                        tags: None,
                        value: None,
                        annotations: None,
                        role: Default::default(),
                    },
                    Attribute {
                        name: "test.enum".to_owned(),
                        r#type: AttributeType::Enum {
                            members: vec![
                                EnumEntriesSpec {
                                    id: "test_enum_member".to_owned(),
                                    value: ValueSpec::String("example_variant1".to_owned()),
                                    brief: None,
                                    note: None,
                                    stability: Some(Stability::Stable),
                                    deprecated: None,
                                    annotations: None,
                                },
                                EnumEntriesSpec {
                                    id: "test_enum_member2".to_owned(),
                                    value: ValueSpec::String("example_variant2".to_owned()),
                                    brief: None,
                                    note: None,
                                    stability: Some(Stability::Stable),
                                    deprecated: None,
                                    annotations: None,
                                },
                            ],
                        },
                        examples: None,
                        brief: "".to_owned(),
                        tag: None,
                        requirement_level: RequirementLevel::Recommended {
                            text: "".to_owned(),
                        },
                        sampling_relevant: None,
                        note: "".to_owned(),
                        stability: Some(Stability::Stable),
                        deprecated: None,
                        prefix: false,
                        tags: None,
                        value: None,
                        annotations: None,
                        role: Default::default(),
                    },
                    Attribute {
                        name: "test.deprecated".to_owned(),
                        r#type: AttributeType::PrimitiveOrArray(PrimitiveOrArrayTypeSpec::String),
                        examples: Some(Examples::Strings(vec![
                            "value1".to_owned(),
                            "value2".to_owned(),
                        ])),
                        brief: "".to_owned(),
                        tag: None,
                        requirement_level: RequirementLevel::Recommended {
                            text: "".to_owned(),
                        },
                        sampling_relevant: None,
                        note: "".to_owned(),
                        stability: Some(Stability::Development),
                        deprecated: Some(weaver_semconv::deprecated::Deprecated::Uncategorized {
                            note: "note".to_owned(),
                        }),
                        prefix: false,
                        tags: None,
                        value: None,
                        annotations: None,
                        role: Default::default(),
                    },
                    Attribute {
                        name: "test.template".to_owned(),
                        r#type: AttributeType::Template(TemplateTypeSpec::String),
                        examples: Some(Examples::Strings(vec![
                            "value1".to_owned(),
                            "value2".to_owned(),
                        ])),
                        brief: "".to_owned(),
                        tag: None,
                        requirement_level: RequirementLevel::Recommended {
                            text: "".to_owned(),
                        },
                        sampling_relevant: None,
                        note: "".to_owned(),
                        stability: Some(Stability::Stable),
                        deprecated: None,
                        prefix: false,
                        tags: None,
                        value: None,
                        annotations: None,
                        role: Default::default(),
                    },
                ],
                span_kind: Some(SpanKindSpec::Internal),
                events: vec![],
                metric_name: None,
                instrument: None,
                unit: None,
                name: None,
                lineage: None,
                display_name: None,
                body: None,
                annotations: None,
            }],
        }
    }

    fn make_metrics_registry() -> ResolvedRegistry {
        ResolvedRegistry {
            registry_url: "TEST_METRICS".to_owned(),
            groups: vec![
                // Attribute group for system memory
                ResolvedGroup {
                    id: "registry.system.memory".to_owned(),
                    r#type: GroupType::AttributeGroup,
                    brief: "Describes System Memory attributes".to_owned(),
                    note: "".to_owned(),
                    prefix: "".to_owned(),
                    entity_associations: vec![],
                    extends: None,
                    stability: None,
                    deprecated: None,
                    attributes: vec![Attribute {
                        name: "system.memory.state".to_owned(),
                        r#type: AttributeType::Enum {
                            members: vec![
                                EnumEntriesSpec {
                                    id: "used".to_owned(),
                                    value: ValueSpec::String("used".to_owned()),
                                    brief: None,
                                    note: None,
                                    stability: Some(Stability::Development),
                                    deprecated: None,
                                    annotations: None,
                                },
                                EnumEntriesSpec {
                                    id: "free".to_owned(),
                                    value: ValueSpec::String("free".to_owned()),
                                    brief: None,
                                    note: None,
                                    stability: Some(Stability::Development),
                                    deprecated: None,
                                    annotations: None,
                                },
                            ],
                        },
                        examples: Some(Examples::Strings(vec![
                            "free".to_owned(),
                            "cached".to_owned(),
                        ])),
                        brief: "The memory state".to_owned(),
                        tag: None,
                        requirement_level: RequirementLevel::Recommended {
                            text: "".to_owned(),
                        },
                        sampling_relevant: None,
                        note: "".to_owned(),
                        stability: Some(Stability::Development),
                        deprecated: None,
                        prefix: false,
                        tags: None,
                        value: None,
                        annotations: None,
                        role: Default::default(),
                    }],
                    span_kind: None,
                    events: vec![],
                    metric_name: None,
                    instrument: None,
                    unit: None,
                    name: None,
                    lineage: None,
                    display_name: Some("System Memory Attributes".to_owned()),
                    body: None,
                    annotations: None,
                },
                // System uptime metric
                ResolvedGroup {
                    id: "metric.system.uptime".to_owned(),
                    r#type: GroupType::Metric,
                    brief: "The time the system has been running".to_owned(),
                    note: "".to_owned(),
                    prefix: "".to_owned(),
                    entity_associations: vec![],
                    extends: None,
                    stability: Some(Stability::Development),
                    deprecated: None,
                    attributes: vec![],
                    span_kind: None,
                    events: vec![],
                    metric_name: Some("system.uptime".to_owned()),
                    instrument: Some(InstrumentSpec::Gauge),
                    unit: Some("s".to_owned()),
                    name: None,
                    lineage: None,
                    display_name: None,
                    body: None,
                    annotations: None,
                },
                // System memory usage metric
                ResolvedGroup {
                    id: "metric.system.memory.usage".to_owned(),
                    r#type: GroupType::Metric,
                    brief: "Reports memory in use by state.".to_owned(),
                    note: "".to_owned(),
                    prefix: "".to_owned(),
                    entity_associations: vec![],
                    extends: None,
                    stability: Some(Stability::Development),
                    deprecated: None,
                    attributes: vec![Attribute {
                        name: "system.memory.state".to_owned(),
                        r#type: AttributeType::PrimitiveOrArray(PrimitiveOrArrayTypeSpec::String),
                        examples: None,
                        brief: "The memory state".to_owned(),
                        tag: None,
                        requirement_level: RequirementLevel::Recommended {
                            text: "".to_owned(),
                        },
                        sampling_relevant: None,
                        note: "".to_owned(),
                        stability: Some(Stability::Development),
                        deprecated: None,
                        prefix: false,
                        tags: None,
                        value: None,
                        annotations: None,
                        role: Default::default(),
                    }],
                    span_kind: None,
                    events: vec![],
                    metric_name: Some("system.memory.usage".to_owned()),
                    instrument: Some(InstrumentSpec::UpDownCounter),
                    unit: Some("By".to_owned()),
                    name: None,
                    lineage: None,
                    display_name: None,
                    body: None,
                    annotations: None,
                },
            ],
        }
    }

    #[test]
    fn test_custom_rego() {
        let registry = ResolvedRegistry {
            registry_url: "TEST".to_owned(),
            groups: vec![ResolvedGroup {
                id: "custom.comprehensive.internal".to_owned(),
                r#type: GroupType::Span,
                brief: "".to_owned(),
                note: "".to_owned(),
                prefix: "".to_owned(),
                entity_associations: vec![],
                extends: None,
                stability: Some(Stability::Stable),
                deprecated: None,
                attributes: vec![Attribute {
                    name: "custom.string".to_owned(),
                    r#type: AttributeType::PrimitiveOrArray(PrimitiveOrArrayTypeSpec::String),
                    examples: Some(Examples::Strings(vec![
                        "value1".to_owned(),
                        "value2".to_owned(),
                    ])),
                    brief: "".to_owned(),
                    tag: None,
                    requirement_level: RequirementLevel::Recommended {
                        text: "".to_owned(),
                    },
                    sampling_relevant: None,
                    note: "".to_owned(),
                    stability: Some(Stability::Stable),
                    deprecated: None,
                    prefix: false,
                    tags: None,
                    value: None,
                    annotations: None,
                    role: Default::default(),
                }],
                span_kind: Some(SpanKindSpec::Internal),
                events: vec![],
                metric_name: None,
                instrument: None,
                unit: None,
                name: None,
                lineage: None,
                display_name: None,
                body: None,
                annotations: None,
            }],
        };

        let mut samples = vec![
            Sample::Attribute(SampleAttribute::try_from("custom.string=hello").unwrap()),
            Sample::Attribute(SampleAttribute::try_from("test.string").unwrap()),
        ];

        let advisors: Vec<Box<dyn Advisor>> = vec![];

        let mut live_checker = LiveChecker::new(registry, advisors);
        let rego_advisor = RegoAdvisor::new(
            &live_checker,
            &Some("data/policies/live_check_advice/".into()),
            &Some("data/jq/test.jq".into()),
        )
        .expect("Failed to create Rego advisor");
        live_checker.add_advisor(Box::new(rego_advisor));

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            let result = sample.run_live_check(&mut live_checker, &mut stats, None);
            assert!(result.is_ok());
        }
        stats.finalize();

        let all_advice = get_all_advice(&mut samples[0]);
        assert!(all_advice.is_empty());

        let all_advice = get_all_advice(&mut samples[1]);
        assert_eq!(all_advice.len(), 2);

        assert_eq!(all_advice[0].advice_type, "missing_attribute");
        assert_eq!(all_advice[0].value, Value::String("test.string".to_owned()));
        assert_eq!(all_advice[0].message, "Does not exist in the registry");
        assert_eq!(all_advice[1].advice_type, "contains_test");
        assert_eq!(all_advice[1].value, Value::String("test.string".to_owned()));
        assert_eq!(all_advice[1].message, "Name must not contain 'test'");

        // Check statistics
        assert_eq!(stats.total_entities, 2);
        assert_eq!(stats.total_advisories, 2);
        assert_eq!(stats.advice_level_counts.len(), 1);
        assert_eq!(stats.advice_level_counts[&AdviceLevel::Violation], 2);
        assert_eq!(stats.highest_advice_level_counts.len(), 1);
        assert_eq!(
            stats.highest_advice_level_counts[&AdviceLevel::Violation],
            1
        );
        assert_eq!(stats.no_advice_count, 1);
    }

    #[test]
    fn test_json_input_output() {
        let registry = make_registry();

        // Load samples from JSON file
        let path = "data/span.json";
        let mut samples: Vec<Sample> =
            serde_json::from_reader(File::open(path).expect("Unable to open file"))
                .expect("Unable to parse JSON");

        let advisors: Vec<Box<dyn Advisor>> = vec![
            Box::new(DeprecatedAdvisor),
            Box::new(StabilityAdvisor),
            Box::new(TypeAdvisor),
            Box::new(EnumAdvisor),
        ];

        let mut live_checker = LiveChecker::new(registry, advisors);
        let rego_advisor =
            RegoAdvisor::new(&live_checker, &None, &None).expect("Failed to create Rego advisor");
        live_checker.add_advisor(Box::new(rego_advisor));

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            let result = sample.run_live_check(&mut live_checker, &mut stats, None);
            assert!(result.is_ok());
        }
        stats.finalize();

        // Check the statistics
        assert_eq!(stats.total_entities, 14);
        assert_eq!(stats.total_entities_by_type.get("attribute"), Some(&10));
        assert_eq!(stats.total_entities_by_type.get("span"), Some(&1));
        assert_eq!(stats.total_entities_by_type.get("span_event"), Some(&1));
        assert_eq!(stats.total_entities_by_type.get("span_link"), Some(&1));
        assert_eq!(stats.total_entities_by_type.get("resource"), Some(&1));
        assert_eq!(stats.total_advisories, 14);
    }

    #[test]
    fn test_json_span_rego() {
        let registry = make_registry();

        // Load samples from JSON file
        let path = "data/span.json";
        let mut samples: Vec<Sample> =
            serde_json::from_reader(File::open(path).expect("Unable to open file"))
                .expect("Unable to parse JSON");

        let mut live_checker = LiveChecker::new(registry, vec![]);
        let rego_advisor = RegoAdvisor::new(
            &live_checker,
            &Some("data/policies/live_check_advice/".into()),
            &Some("data/jq/test.jq".into()),
        )
        .expect("Failed to create Rego advisor");
        live_checker.add_advisor(Box::new(rego_advisor));

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            let result = sample.run_live_check(&mut live_checker, &mut stats, None);
            assert!(result.is_ok());
        }
        stats.finalize();

        // Check the statistics
        assert_eq!(
            stats.advice_type_counts.get("contains_test_in_status"),
            Some(&1)
        );
    }

    #[test]
    fn test_json_metric() {
        let registry = make_metrics_registry();

        // Load samples from JSON file
        let path = "data/metrics.json";
        let mut samples: Vec<Sample> =
            serde_json::from_reader(File::open(path).expect("Unable to open file"))
                .expect("Unable to parse JSON");

        let advisors: Vec<Box<dyn Advisor>> = vec![
            Box::new(DeprecatedAdvisor),
            Box::new(StabilityAdvisor),
            Box::new(TypeAdvisor),
            Box::new(EnumAdvisor),
        ];

        let mut live_checker = LiveChecker::new(registry, advisors);
        let rego_advisor =
            RegoAdvisor::new(&live_checker, &None, &None).expect("Failed to create Rego advisor");
        live_checker.add_advisor(Box::new(rego_advisor));

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            let result = sample.run_live_check(&mut live_checker, &mut stats, None);
            assert!(result.is_ok());
        }
        stats.finalize();

        // Check the statistics
        assert_eq!(stats.total_entities_by_type.get("data_point"), Some(&6));
        assert_eq!(stats.total_entities_by_type.get("metric"), Some(&4));
        assert_eq!(stats.total_entities_by_type.get("attribute"), Some(&3));
        assert_eq!(stats.no_advice_count, 4);
        assert_eq!(
            stats
                .advice_type_counts
                .get("recommended_attribute_not_present"),
            Some(&2)
        );
        assert_eq!(stats.advice_type_counts.get("missing_attribute"), Some(&2));
        assert_eq!(stats.advice_type_counts.get("stability"), Some(&2));
        assert_eq!(stats.advice_type_counts.get("missing_metric"), Some(&3));
        assert_eq!(stats.advice_type_counts.get("missing_namespace"), Some(&2));
        assert_eq!(
            stats.seen_registry_metrics.get("system.memory.usage"),
            Some(&1)
        );
        assert_eq!(stats.seen_non_registry_metrics.len(), 3);
    }

    #[test]
    fn test_json_metric_custom_rego() {
        let registry = make_metrics_registry();

        // Load samples from JSON file
        let path = "data/metrics.json";
        let mut samples: Vec<Sample> =
            serde_json::from_reader(File::open(path).expect("Unable to open file"))
                .expect("Unable to parse JSON");

        let mut live_checker = LiveChecker::new(registry, vec![]);
        let rego_advisor = RegoAdvisor::new(
            &live_checker,
            &Some("data/policies/live_check_advice/".into()),
            &Some("data/jq/test.jq".into()),
        )
        .expect("Failed to create Rego advisor");
        live_checker.add_advisor(Box::new(rego_advisor));

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            let result = sample.run_live_check(&mut live_checker, &mut stats, None);

            assert!(result.is_ok());
        }
        stats.finalize();
        assert_eq!(
            stats.advice_type_counts.get("invalid_data_point_value"),
            Some(&1)
        );
    }

    #[test]
    fn test_bad_custom_rego() {
        let registry = ResolvedRegistry {
            registry_url: "TEST".to_owned(),
            groups: vec![ResolvedGroup {
                id: "custom.comprehensive.internal".to_owned(),
                r#type: GroupType::Span,
                brief: "".to_owned(),
                note: "".to_owned(),
                prefix: "".to_owned(),
                entity_associations: vec![],
                extends: None,
                stability: Some(Stability::Stable),
                deprecated: None,
                attributes: vec![Attribute {
                    name: "custom.string".to_owned(),
                    r#type: AttributeType::PrimitiveOrArray(PrimitiveOrArrayTypeSpec::String),
                    examples: Some(Examples::Strings(vec![
                        "value1".to_owned(),
                        "value2".to_owned(),
                    ])),
                    brief: "".to_owned(),
                    tag: None,
                    requirement_level: RequirementLevel::Recommended {
                        text: "".to_owned(),
                    },
                    sampling_relevant: None,
                    note: "".to_owned(),
                    stability: Some(Stability::Stable),
                    deprecated: None,
                    prefix: false,
                    tags: None,
                    value: None,
                    annotations: None,
                    role: Default::default(),
                }],
                span_kind: Some(SpanKindSpec::Internal),
                events: vec![],
                metric_name: None,
                instrument: None,
                unit: None,
                name: None,
                lineage: None,
                display_name: None,
                body: None,
                annotations: None,
            }],
        };

        let mut samples = vec![Sample::Attribute(
            SampleAttribute::try_from("custom.string=hello").unwrap(),
        )];

        let advisors: Vec<Box<dyn Advisor>> = vec![];

        let mut live_checker = LiveChecker::new(registry, advisors);
        let rego_advisor = RegoAdvisor::new(
            &live_checker,
            &Some("data/policies/bad_advice/".into()),
            &Some("data/jq/test.jq".into()),
        )
        .expect("Failed to create Rego advisor");
        live_checker.add_advisor(Box::new(rego_advisor));

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            // This should fail with: "error: use of undefined variable `attribu1te_name` is unsafe"

            let result = sample.run_live_check(&mut live_checker, &mut stats, None);
            assert!(result.is_err());
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("use of undefined variable"));
        }
    }

    #[test]
    fn test_exponential_histogram() {
        let registry = make_metrics_registry();

        // A sample with exponential histogram data points
        let sample = Sample::Metric(SampleMetric {
            name: "system.memory.usage".to_owned(),
            instrument: SampleInstrument::Supported(InstrumentSpec::Histogram),
            unit: "By".to_owned(),
            data_points: Some(DataPoints::ExponentialHistogram(vec![
                SampleExponentialHistogramDataPoint {
                    attributes: vec![],
                    count: 0,
                    sum: None,
                    min: None,
                    max: None,
                    live_check_result: None,
                    scale: 1,
                    zero_count: 0,
                    positive: None,
                    negative: None,
                    flags: 0,
                    zero_threshold: 0.0,
                    exemplars: vec![],
                },
            ])),
            live_check_result: None,
        });
        let mut samples = vec![sample];
        let advisors: Vec<Box<dyn Advisor>> = vec![Box::new(TypeAdvisor)];
        let mut live_checker = LiveChecker::new(registry, advisors);

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            let result = sample.run_live_check(&mut live_checker, &mut stats, None);
            assert!(result.is_ok());
        }
        stats.finalize();
        assert_eq!(
            stats.advice_type_counts.get("instrument_mismatch"),
            Some(&1)
        );
        // Check the live check result for the sample has the correct instrument mismatch message
        let sample = match &samples[0] {
            Sample::Metric(m) => m,
            _ => panic!("Expected a Metric sample"),
        };
        let live_check_result = sample.live_check_result.as_ref().unwrap();
        // Get the instrument_mismatch from all_advice
        let advice = live_check_result
            .all_advice
            .iter()
            .find(|a| a.advice_type == "instrument_mismatch")
            .expect("Expected instrument_mismatch advice");
        assert_eq!(advice.message, "Instrument should be `updowncounter`");
    }

    #[test]
    fn test_gauge_exemplar_rego() {
        let registry = make_metrics_registry();

        // A gauge sample with an exemplar
        let mut sample = Sample::Metric(SampleMetric {
            name: "system.uptime".to_owned(),
            instrument: SampleInstrument::Supported(InstrumentSpec::Gauge),
            unit: "s".to_owned(),
            data_points: Some(DataPoints::Number(vec![SampleNumberDataPoint {
                attributes: vec![],
                value: json!(0.0),
                flags: 0,
                live_check_result: None,
                exemplars: vec![SampleExemplar {
                    timestamp: "".to_owned(),
                    value: json!(0.0),
                    filtered_attributes: vec![],
                    span_id: "".to_owned(),
                    trace_id: "".to_owned(),
                    live_check_result: None,
                }],
            }])),
            live_check_result: None,
        });
        let advisors: Vec<Box<dyn Advisor>> = vec![Box::new(TypeAdvisor)];
        let mut live_checker = LiveChecker::new(registry, advisors);

        let rego_advisor = RegoAdvisor::new(
            &live_checker,
            &Some("data/policies/live_check_advice/".into()),
            &Some("data/jq/test.jq".into()),
        )
        .expect("Failed to create Rego advisor");
        live_checker.add_advisor(Box::new(rego_advisor));

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        let result = sample.run_live_check(&mut live_checker, &mut stats, None);

        assert!(result.is_ok());
        stats.finalize();
        assert_eq!(stats.advice_type_counts.get("low_value"), Some(&1));
    }

    #[test]
    fn test_summary_unspecified() {
        let registry = make_metrics_registry();

        let mut samples = vec![
            Sample::Metric(SampleMetric {
                name: "system.memory.usage".to_owned(),
                instrument: SampleInstrument::Unsupported("Summary".to_owned()),
                unit: "By".to_owned(),
                data_points: None,
                live_check_result: None,
            }),
            Sample::Metric(SampleMetric {
                name: "system.memory.usage".to_owned(),
                instrument: SampleInstrument::Unsupported("Unspecified".to_owned()),
                unit: "By".to_owned(),
                data_points: None,
                live_check_result: None,
            }),
        ];
        let advisors: Vec<Box<dyn Advisor>> = vec![Box::new(TypeAdvisor)];
        let mut live_checker = LiveChecker::new(registry, advisors);

        let mut stats = LiveCheckStatistics::new(&live_checker.registry);
        for sample in &mut samples {
            let result = sample.run_live_check(&mut live_checker, &mut stats, None);
            assert!(result.is_ok());
        }
        stats.finalize();
        assert_eq!(
            stats.advice_type_counts.get("unsupported_instrument"),
            Some(&2)
        );
    }
}
