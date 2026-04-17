//! Graph traversal and spatial analysis for MemPalace.
//!
//! BFS traversal, tunnel discovery, and graph statistics.
//! Separated from the core Palace structure for single-responsibility.

use std::collections::{HashMap, HashSet, VecDeque};

use super::palace::{GraphStats, Palace, TraversalNode, TunnelInfo};

impl Palace {
    /// BFS traversal from a starting room.
    ///
    /// Finds connected rooms through shared wings, up to max_hops away.
    /// Uses `VecDeque` for O(1) front-pop instead of `Vec::remove(0)`.
    #[allow(dead_code)]
    pub fn traverse(&self, start_room: &str, max_hops: usize) -> Vec<TraversalNode> {
        // Build room → (wings, halls, item_count) mapping
        let mut room_data: HashMap<String, (Vec<String>, Vec<String>, usize)> = HashMap::new();
        for wing in self.wings.values() {
            for room in wing.rooms.values() {
                let entry = room_data
                    .entry(room.id.clone())
                    .or_insert_with(|| (Vec::new(), Vec::new(), 0));
                if !entry.0.contains(&wing.id) {
                    entry.0.push(wing.id.clone());
                }
                entry.2 += room.drawer_count + room.hall_count;
            }
        }

        if !room_data.contains_key(start_room) {
            return Vec::new();
        }

        let mut visited = HashSet::new();
        visited.insert(start_room.to_string());

        let start_data = &room_data[start_room];
        let mut results = vec![TraversalNode {
            room: start_room.to_string(),
            wings: start_data.0.clone(),
            halls: Vec::new(),
            count: start_data.2,
            hop: 0,
            connected_via: None,
        }];

        // Use VecDeque for O(1) pop_front (Problem 11 fix)
        let mut frontier: VecDeque<(String, usize)> = VecDeque::new();
        frontier.push_back((start_room.to_string(), 0));

        while let Some((current_room, depth)) = frontier.pop_front() {
            if depth >= max_hops {
                continue;
            }

            let current_wings: HashSet<String> = room_data
                .get(&current_room)
                .map(|d| d.0.iter().cloned().collect())
                .unwrap_or_default();

            for (room, data) in &room_data {
                if visited.contains(room) {
                    continue;
                }
                let room_wings: HashSet<String> = data.0.iter().cloned().collect();
                let shared: Vec<String> = current_wings
                    .intersection(&room_wings)
                    .cloned()
                    .collect();

                if !shared.is_empty() {
                    visited.insert(room.clone());
                    results.push(TraversalNode {
                        room: room.clone(),
                        wings: data.0.clone(),
                        halls: Vec::new(),
                        count: data.2,
                        hop: depth + 1,
                        connected_via: Some(shared),
                    });
                    if depth + 1 < max_hops {
                        frontier.push_back((room.clone(), depth + 1));
                    }
                }
            }
        }

        // Sort by hop distance, then by count descending
        results.sort_by(|a, b| a.hop.cmp(&b.hop).then(b.count.cmp(&a.count)));
        results.truncate(50);
        results
    }

    /// Find rooms that bridge two wings (tunnel rooms).
    #[allow(dead_code)]
    pub fn find_tunnels(
        &self,
        wing_a: Option<&str>,
        wing_b: Option<&str>,
    ) -> Vec<TunnelInfo> {
        let mut tunnel_rooms = Vec::new();

        for (room_name, tunnel) in &self.tunnels {
            if tunnel.connected_wings.len() < 2 {
                continue;
            }
            if let Some(wa) = wing_a {
                if !tunnel.connected_wings.contains(&wa.to_string()) {
                    continue;
                }
            }
            if let Some(wb) = wing_b {
                if !tunnel.connected_wings.contains(&wb.to_string()) {
                    continue;
                }
            }

            // Count total drawers across all wings for this room
            let count: usize = self
                .wings
                .values()
                .filter_map(|w| w.rooms.get(room_name))
                .map(|r| r.drawer_count)
                .sum();

            tunnel_rooms.push(TunnelInfo {
                room: room_name.clone(),
                wings: tunnel.connected_wings.clone(),
                count,
            });
        }

        tunnel_rooms.sort_by(|a, b| b.count.cmp(&a.count));
        tunnel_rooms.truncate(50);
        tunnel_rooms
    }

    /// Graph statistics summary.
    #[allow(dead_code)]
    pub fn graph_stats(&self) -> GraphStats {
        let total_rooms: usize = self.wings.values().map(|w| w.rooms.len()).sum();
        let tunnel_rooms = self
            .tunnels
            .values()
            .filter(|t| t.connected_wings.len() >= 2)
            .count();

        let mut rooms_per_wing = HashMap::new();
        for wing in self.wings.values() {
            rooms_per_wing.insert(wing.id.clone(), wing.rooms.len());
        }

        let mut top_tunnels: Vec<TunnelInfo> = self
            .tunnels
            .iter()
            .filter(|(_, t)| t.connected_wings.len() >= 2)
            .map(|(name, t)| {
                let count: usize = self
                    .wings
                    .values()
                    .filter_map(|w| w.rooms.get(name))
                    .map(|r| r.drawer_count)
                    .sum();
                TunnelInfo {
                    room: name.clone(),
                    wings: t.connected_wings.clone(),
                    count,
                }
            })
            .collect();
        top_tunnels.sort_by(|a, b| b.wings.len().cmp(&a.wings.len()));
        top_tunnels.truncate(10);

        // Count edges (room shared across wings)
        let total_edges: usize = self
            .tunnels
            .values()
            .map(|t| {
                let n = t.connected_wings.len();
                if n >= 2 { n * (n - 1) / 2 } else { 0 }
            })
            .sum();

        GraphStats {
            total_rooms,
            tunnel_rooms,
            total_edges,
            rooms_per_wing,
            top_tunnels,
        }
    }
}
