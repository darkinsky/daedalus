//! Knowledge Graph query operations for MemPalace.
//!
//! Provides entity queries, relationship queries, timeline views,
//! and KG statistics. Separated from the core Palace structure
//! for single-responsibility and testability.

use std::collections::HashMap;

use super::palace::{Entity, EntityFacts, KgDirection, KgFact, KgStats, Palace, Triple};

// ── KG Query Operations ──

impl Palace {
    /// Query all relationships for an entity.
    ///
    /// Supports direction filtering (outgoing, incoming, both) and
    /// temporal filtering (as_of date).
    #[allow(dead_code)]
    pub fn query_entity(
        &self,
        name: &str,
        as_of: Option<&str>,
        direction: KgDirection,
    ) -> Vec<KgFact> {
        let entity_lower = name.to_lowercase();
        let mut results = Vec::new();

        if direction == KgDirection::Outgoing || direction == KgDirection::Both {
            results.extend(
                self.triples.iter()
                    .filter(|t| t.subject.to_lowercase() == entity_lower)
                    .filter(|t| as_of.map_or(true, |date| t.is_valid_at(date)))
                    .map(|t| t.to_kg_fact("outgoing"))
            );
        }

        if direction == KgDirection::Incoming || direction == KgDirection::Both {
            results.extend(
                self.triples.iter()
                    .filter(|t| t.object.to_lowercase() == entity_lower)
                    .filter(|t| as_of.map_or(true, |date| t.is_valid_at(date)))
                    .map(|t| t.to_kg_fact("incoming"))
            );
        }

        results
    }

    /// Query all triples with a given relationship type.
    #[allow(dead_code)]
    pub fn query_relationship(&self, predicate: &str, as_of: Option<&str>) -> Vec<KgFact> {
        let pred_lower = predicate.to_lowercase().replace(' ', "_");
        self.triples
            .iter()
            .filter(|t| t.predicate.to_lowercase().replace(' ', "_") == pred_lower)
            .filter(|t| as_of.map_or(true, |date| t.is_valid_at(date)))
            .map(|t| t.to_kg_fact("outgoing"))
            .collect()
    }

    /// Get chronological timeline of facts, optionally filtered by entity.
    #[allow(dead_code)]
    pub fn timeline(&self, entity_name: Option<&str>) -> Vec<KgFact> {
        let mut facts: Vec<KgFact> = self
            .triples
            .iter()
            .filter(|t| {
                entity_name.map_or(true, |name| {
                    let name_lower = name.to_lowercase();
                    t.subject.to_lowercase() == name_lower
                        || t.object.to_lowercase() == name_lower
                })
            })
            .map(|t| t.to_kg_fact("outgoing"))
            .collect();

        // Sort by valid_from ascending (None last)
        facts.sort_by(|a, b| {
            let a_date = a.valid_from.as_deref().unwrap_or("9999-99-99");
            let b_date = b.valid_from.as_deref().unwrap_or("9999-99-99");
            a_date.cmp(b_date)
        });

        // Cap at 100 results
        facts.truncate(100);
        facts
    }

    /// Get knowledge graph statistics.
    #[allow(dead_code)]
    pub fn kg_stats(&self) -> KgStats {
        let current = self.triples.iter().filter(|t| t.is_valid()).count();
        let expired = self.triples.len() - current;
        let mut relationship_types: Vec<String> = self
            .triples
            .iter()
            .map(|t| t.predicate.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        relationship_types.sort();

        KgStats {
            entities: self.entities.len(),
            triples: self.triples.len(),
            current_facts: current,
            expired_facts: expired,
            relationship_types,
        }
    }

    /// Seed the knowledge graph from entity facts.
    ///
    /// Bootstraps the graph with known ground truth from a structured
    /// facts dictionary. Matches the original MemPalace
    /// `seed_from_entity_facts()` method.
    #[allow(dead_code)]
    pub fn seed_from_entity_facts(&mut self, facts: &HashMap<String, EntityFacts>) {
        for (key, fact) in facts {
            let name = fact.full_name.clone().unwrap_or_else(|| key.clone());
            let etype = fact.entity_type.clone().unwrap_or_else(|| "person".to_string());

            let mut props = HashMap::new();
            if let Some(ref gender) = fact.gender {
                props.insert("gender".to_string(), gender.clone());
            }
            if let Some(ref birthday) = fact.birthday {
                props.insert("birthday".to_string(), birthday.clone());
            }

            let entity = Entity::with_properties(name.clone(), etype.clone(), props);
            self.add_entity(entity);

            // Relationships
            if let Some(ref parent) = fact.parent {
                let mut t = Triple::new(
                    name.clone(),
                    "child_of".to_string(),
                    parent.clone(),
                    "seed".to_string(),
                    "seed".to_string(),
                );
                t.valid_from = fact.birthday.clone();
                self.add_triple(t);
            }

            if let Some(ref partner) = fact.partner {
                self.add_triple(Triple::new(
                    name.clone(),
                    "married_to".to_string(),
                    partner.clone(),
                    "seed".to_string(),
                    "seed".to_string(),
                ));
            }

            // Interests
            for interest in &fact.interests {
                let mut t = Triple::new(
                    name.clone(),
                    "loves".to_string(),
                    interest.clone(),
                    "seed".to_string(),
                    "seed".to_string(),
                );
                t.valid_from = Some("2025-01-01".to_string());
                self.add_triple(t);
            }
        }
    }

    /// Get all valid triples related to a given entity (as subject or object).
    ///
    /// Uses exact match (case-insensitive) to avoid false positives
    /// like "max" matching "maximum".
    pub fn find_related_triples(&self, entity: &str) -> Vec<&Triple> {
        let entity_lower = entity.to_lowercase();
        self.triples
            .iter()
            .filter(|t| t.is_valid())
            .filter(|t| {
                t.subject.to_lowercase() == entity_lower
                    || t.object.to_lowercase() == entity_lower
            })
            .collect()
    }
}

// ── Helper: Triple → KgFact conversion ──

impl Triple {
    /// Convert this triple to a KgFact for query results.
    #[allow(dead_code)]
    pub(super) fn to_kg_fact(&self, direction: &str) -> KgFact {
        KgFact {
            direction: direction.to_string(),
            subject: self.subject.clone(),
            predicate: self.predicate.clone(),
            object: self.object.clone(),
            valid_from: self.valid_from.clone(),
            valid_to: self.valid_to.clone(),
            confidence: self.confidence,
            source_closet: self.source_closet.clone(),
            current: self.is_valid(),
        }
    }
}
