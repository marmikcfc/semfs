//! L2/L3 — graphify-style community detection over the file↔file projection.
//!
//! Pure, I/O-free, deterministic (fixed node order, no RNG) so it is unit-testable
//! and reproducible across runs. The detector sits behind [`CommunityDetector`]
//! so the Louvain core can be swapped for / refined into Leiden without touching
//! callers (SOLID; see `tickets/ls-kg-semantic-readdir/graphify_kg_architecture.md`).
//!
//! Pipeline: bipartite file↔entity edges → weighted file↔file graph
//! (`weight(a,b) = #shared entities`) → Louvain modularity optimization →
//! Leiden-style well-connectedness refinement (split internally-disconnected
//! communities) → per-community god-nodes (highest-degree entities, p99 hubs
//! excluded so a corpus-wide entity doesn't define every topic).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// A weighted, undirected graph over `n` nodes (0..n), node = a file.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub n: usize,
    /// adj[i] = list of (neighbor, weight); symmetric.
    pub adj: Vec<Vec<(usize, f64)>>,
}

impl Graph {
    pub fn new(n: usize) -> Self {
        Graph {
            n,
            adj: vec![Vec::new(); n],
        }
    }

    /// Build a file↔file graph from bipartite (file, entity) memberships:
    /// `weight(a,b) = number of entities both files link to`.
    /// `file_entities[i]` = the set of entity ids file `i` mentions.
    pub fn from_file_entities(file_entities: &[HashSet<u32>]) -> Self {
        let n = file_entities.len();
        // entity -> files mentioning it (inverted index)
        let mut by_entity: HashMap<u32, Vec<usize>> = HashMap::new();
        for (fi, ents) in file_entities.iter().enumerate() {
            for &e in ents {
                by_entity.entry(e).or_default().push(fi);
            }
        }
        // accumulate shared-entity counts per file pair
        let mut wmap: HashMap<(usize, usize), f64> = HashMap::new();
        for files in by_entity.values() {
            // skip ubiquitous entities that would densely connect everything;
            // a star over >N files contributes no community signal, only cost.
            if files.len() < 2 || files.len() > 64 {
                continue;
            }
            for i in 0..files.len() {
                for j in (i + 1)..files.len() {
                    let (a, b) = (files[i].min(files[j]), files[i].max(files[j]));
                    *wmap.entry((a, b)).or_insert(0.0) += 1.0;
                }
            }
        }
        let mut g = Graph::new(n);
        for ((a, b), w) in wmap {
            g.adj[a].push((b, w));
            g.adj[b].push((a, w));
        }
        // deterministic neighbor order
        for nb in &mut g.adj {
            nb.sort_by(|x, y| x.0.cmp(&y.0));
        }
        g
    }

    fn weighted_degree(&self, i: usize) -> f64 {
        self.adj[i].iter().map(|(_, w)| w).sum()
    }

    fn total_weight(&self) -> f64 {
        (0..self.n).map(|i| self.weighted_degree(i)).sum::<f64>() / 2.0
    }
}

/// A community detector — Louvain today, Leiden-refined behind the same trait.
pub trait CommunityDetector {
    /// Returns a community id (densely numbered 0..k) per node.
    fn detect(&self, g: &Graph, resolution: f64) -> Vec<usize>;
}

/// Louvain modularity maximization + a Leiden-style well-connectedness
/// refinement pass (splits any community that is internally disconnected).
#[derive(Debug, Default, Clone, Copy)]
pub struct Louvain {
    /// Run the Leiden refinement (split disconnected communities). Off → plain Louvain.
    pub leiden_refine: bool,
}

impl CommunityDetector for Louvain {
    fn detect(&self, g: &Graph, resolution: f64) -> Vec<usize> {
        let mut comm = louvain_one_level(g, resolution);
        if self.leiden_refine {
            comm = refine_connected(g, &comm);
        }
        densify(&comm)
    }
}

/// One Louvain level: each node starts in its own community; greedily move each
/// node to the neighboring community that yields the largest modularity gain,
/// iterating in fixed node order until no node moves (or a pass cap). One level
/// is sufficient for a sparse corpus graph (~600 nodes); the aggregation level
/// is omitted (YAGNI) — it mainly merges already-tight clusters.
fn louvain_one_level(g: &Graph, resolution: f64) -> Vec<usize> {
    let m = g.total_weight();
    if m <= 0.0 {
        return (0..g.n).collect(); // no edges → every node its own community
    }
    let two_m = 2.0 * m;
    let k: Vec<f64> = (0..g.n).map(|i| g.weighted_degree(i)).collect();
    let mut comm: Vec<usize> = (0..g.n).collect();
    // sum of degrees of nodes in each community
    let mut sigma_tot: Vec<f64> = k.clone();

    let mut improved = true;
    let mut passes = 0;
    while improved && passes < 20 {
        improved = false;
        passes += 1;
        for i in 0..g.n {
            let ci = comm[i];
            // weights from i into each neighboring community
            let mut w_to: BTreeMap<usize, f64> = BTreeMap::new();
            for &(j, w) in &g.adj[i] {
                if j != i {
                    *w_to.entry(comm[j]).or_insert(0.0) += w;
                }
            }
            // remove i from its community
            sigma_tot[ci] -= k[i];
            let w_to_ci = w_to.get(&ci).copied().unwrap_or(0.0);
            // pick the best community (gain = w_to_c - resolution * k_i * sigma_tot_c / 2m)
            let mut best_c = ci;
            let mut best_gain = w_to_ci - resolution * k[i] * sigma_tot[ci] / two_m;
            for (&c, &w_to_c) in &w_to {
                let gain = w_to_c - resolution * k[i] * sigma_tot[c] / two_m;
                // tie-break to the smallest community id for determinism
                if gain > best_gain + 1e-12 || (gain > best_gain - 1e-12 && c < best_c) {
                    best_gain = gain;
                    best_c = c;
                }
            }
            comm[i] = best_c;
            sigma_tot[best_c] += k[i];
            if best_c != ci {
                improved = true;
            }
        }
    }
    comm
}

/// Leiden well-connectedness: a Louvain community can be internally
/// disconnected (a node moved to a community it only weakly touches). Split each
/// community into its connected components so every reported community is a
/// genuinely connected cluster.
fn refine_connected(g: &Graph, comm: &[usize]) -> Vec<usize> {
    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &c) in comm.iter().enumerate() {
        members.entry(c).or_default().push(i);
    }
    let mut out = vec![0usize; g.n];
    let mut next_id = 0usize;
    // iterate communities in id order for determinism
    let mut cids: Vec<usize> = members.keys().copied().collect();
    cids.sort_unstable();
    for c in cids {
        let nodes = &members[&c];
        let set: HashSet<usize> = nodes.iter().copied().collect();
        let mut seen: HashSet<usize> = HashSet::new();
        for &start in nodes {
            if seen.contains(&start) {
                continue;
            }
            // BFS within the community
            let mut q = VecDeque::new();
            q.push_back(start);
            seen.insert(start);
            while let Some(u) = q.pop_front() {
                out[u] = next_id;
                for &(v, _) in &g.adj[u] {
                    if set.contains(&v) && !seen.contains(&v) {
                        seen.insert(v);
                        q.push_back(v);
                    }
                }
            }
            next_id += 1;
        }
    }
    out
}

/// Renumber community ids to a dense 0..k range, ordered by descending size
/// (largest community = 0) for stable, meaningful output.
fn densify(comm: &[usize]) -> Vec<usize> {
    let mut size: HashMap<usize, usize> = HashMap::new();
    for &c in comm {
        *size.entry(c).or_insert(0) += 1;
    }
    let mut order: Vec<usize> = size.keys().copied().collect();
    order.sort_by(|a, b| size[b].cmp(&size[a]).then(a.cmp(b)));
    let remap: HashMap<usize, usize> = order.iter().enumerate().map(|(new, &old)| (old, new)).collect();
    comm.iter().map(|c| remap[c]).collect()
}

/// Modularity of a partition (for tests: Leiden refinement must not lower it
/// materially, and a good partition is > 0).
pub fn modularity(g: &Graph, comm: &[usize], resolution: f64) -> f64 {
    let m = g.total_weight();
    if m <= 0.0 {
        return 0.0;
    }
    let two_m = 2.0 * m;
    let k: Vec<f64> = (0..g.n).map(|i| g.weighted_degree(i)).collect();
    let mut q = 0.0;
    for i in 0..g.n {
        for &(j, w) in &g.adj[i] {
            if comm[i] == comm[j] {
                q += w - resolution * k[i] * k[j] / two_m;
            }
        }
    }
    // diagonal (self-loops) none; the double count over i,j cancels the 1/2m
    q / two_m
}

/// Entity degree = #files mentioning it. The p99 cut drops ubiquitous "hub"
/// entities so a god-node is a *topic-defining* concept, not a corpus-wide one.
/// Returns the set of entity ids considered hubs (to exclude from god-nodes).
pub fn hub_entities(entity_degree: &HashMap<u32, usize>, pctl: f64) -> HashSet<u32> {
    if entity_degree.is_empty() {
        return HashSet::new();
    }
    let mut degs: Vec<usize> = entity_degree.values().copied().collect();
    degs.sort_unstable();
    // 0-based percentile index over (n-1): for n=20, p99 → idx 18 (the value
    // below which 99% fall), so the lone ubiquitous entity above it is the hub.
    let idx = (((degs.len() - 1) as f64) * pctl).floor() as usize;
    let cut = degs[idx.min(degs.len() - 1)];
    entity_degree
        .iter()
        .filter(|&(_, &d)| d > cut)
        .map(|(&e, _)| e)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hs(v: &[u32]) -> HashSet<u32> {
        v.iter().copied().collect()
    }

    #[test]
    fn two_clear_clusters_separate() {
        // files 0,1,2 share entity 100; files 3,4,5 share entity 200; no overlap.
        let fe = vec![
            hs(&[100]),
            hs(&[100]),
            hs(&[100]),
            hs(&[200]),
            hs(&[200]),
            hs(&[200]),
        ];
        let g = Graph::from_file_entities(&fe);
        let comm = Louvain { leiden_refine: true }.detect(&g, 1.0);
        // {0,1,2} in one community, {3,4,5} in another
        assert_eq!(comm[0], comm[1]);
        assert_eq!(comm[1], comm[2]);
        assert_eq!(comm[3], comm[4]);
        assert_eq!(comm[4], comm[5]);
        assert_ne!(comm[0], comm[3]);
    }

    #[test]
    fn deterministic_across_runs() {
        let fe = vec![hs(&[1, 2]), hs(&[2, 3]), hs(&[3, 1]), hs(&[9]), hs(&[9])];
        let g = Graph::from_file_entities(&fe);
        let a = Louvain { leiden_refine: true }.detect(&g, 1.0);
        let b = Louvain { leiden_refine: true }.detect(&g, 1.0);
        assert_eq!(a, b);
    }

    #[test]
    fn refinement_does_not_lower_modularity() {
        let fe = vec![
            hs(&[1]), hs(&[1]), hs(&[1, 2]), hs(&[2]), hs(&[2]),
            hs(&[3]), hs(&[3]), hs(&[3]),
        ];
        let g = Graph::from_file_entities(&fe);
        let plain = Louvain { leiden_refine: false }.detect(&g, 1.0);
        let refined = Louvain { leiden_refine: true }.detect(&g, 1.0);
        let qp = modularity(&g, &plain, 1.0);
        let qr = modularity(&g, &refined, 1.0);
        // refinement only splits disconnected pieces; modularity stays close
        assert!(qr >= qp - 1e-9, "refined {qr} < plain {qp}");
    }

    #[test]
    fn isolated_nodes_each_own_community() {
        let fe = vec![hs(&[1]), hs(&[2]), hs(&[3])]; // no shared entities
        let g = Graph::from_file_entities(&fe);
        let comm = Louvain { leiden_refine: true }.detect(&g, 1.0);
        assert_eq!(comm.iter().copied().collect::<HashSet<_>>().len(), 3);
    }

    #[test]
    fn hub_exclusion_drops_ubiquitous_entity() {
        let mut deg = HashMap::new();
        deg.insert(1u32, 100); // ubiquitous hub
        for e in 2..=20u32 {
            deg.insert(e, 2);
        }
        let hubs = hub_entities(&deg, 0.99);
        assert!(hubs.contains(&1));
        assert!(!hubs.contains(&5));
    }

    #[test]
    fn densify_orders_by_size_desc() {
        // community 7 has 3 members, community 2 has 1 → 7 becomes 0, 2 becomes 1
        let raw = vec![7, 7, 7, 2];
        let d = densify(&raw);
        assert_eq!(d, vec![0, 0, 0, 1]);
    }
}
