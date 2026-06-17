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
    /// Self-loop weight per node (internal community weight after aggregation).
    /// Contributes `2*self_w[i]` to the node's degree. Zero for leaf graphs.
    pub self_w: Vec<f64>,
}

impl Graph {
    pub fn new(n: usize) -> Self {
        Graph {
            n,
            adj: vec![Vec::new(); n],
            self_w: vec![0.0; n],
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

    /// Add a symmetric weighted edge (used when building induced subgraphs for
    /// oversized-community splitting). Assumes `a != b` and both in range.
    pub fn add_edge(&mut self, a: usize, b: usize, w: f64) {
        self.adj[a].push((b, w));
        self.adj[b].push((a, w));
    }

    /// Densify the graph with embedding-kNN edges: connect each node to its `k`
    /// cosine-nearest neighbours. This wires *semantically* related files even when
    /// they share no named entity (the sparse-edge → singleton fix). Symmetric;
    /// weights accumulate into existing edges so a kNN edge that coincides with a
    /// shared-entity edge reinforces it. `embeddings[i]` is file `i`'s vector.
    pub fn add_knn_edges(&mut self, embeddings: &[Vec<f32>], k: usize, weight: f64) {
        let n = self.n.min(embeddings.len());
        if k == 0 || n < 2 {
            return;
        }
        let norms: Vec<f32> = embeddings
            .iter()
            .take(n)
            .map(|v| v.iter().map(|x| x * x).sum::<f32>().sqrt())
            .collect();
        // collect the undirected kNN pairs (dedup symmetric + against self)
        let mut new_edges: std::collections::BTreeSet<(usize, usize)> =
            std::collections::BTreeSet::new();
        for i in 0..n {
            if norms[i] == 0.0 {
                continue;
            }
            let mut sims: Vec<(f32, usize)> = Vec::with_capacity(n);
            for j in 0..n {
                if i == j || norms[j] == 0.0 {
                    continue;
                }
                let dot: f32 = embeddings[i]
                    .iter()
                    .zip(&embeddings[j])
                    .map(|(a, b)| a * b)
                    .sum();
                sims.push((dot / (norms[i] * norms[j]), j));
            }
            // top-k by cosine desc; deterministic tie-break by node index
            sims.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.1.cmp(&b.1))
            });
            for &(_, j) in sims.iter().take(k) {
                new_edges.insert((i.min(j), i.max(j)));
            }
        }
        // apply: accumulate weight into an existing edge, else add it
        let mut bump = |adj: &mut Vec<(usize, f64)>, nb: usize| match adj
            .iter_mut()
            .find(|(x, _)| *x == nb)
        {
            Some(e) => e.1 += weight,
            None => adj.push((nb, weight)),
        };
        for (a, b) in new_edges {
            bump(&mut self.adj[a], b);
            bump(&mut self.adj[b], a);
        }
        for nb in &mut self.adj {
            nb.sort_by(|x, y| x.0.cmp(&y.0)); // deterministic neighbour order
        }
    }

    fn weighted_degree(&self, i: usize) -> f64 {
        self.adj[i].iter().map(|(_, w)| w).sum::<f64>()
            + 2.0 * self.self_w.get(i).copied().unwrap_or(0.0)
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
            // graphify parity: a community that is too large to be a useful topic
            // is recursively re-clustered (graphify `_MAX_COMMUNITY_FRACTION=0.25`,
            // `_MIN_SPLIT_SIZE=10`). Off when refinement is off (plain Louvain).
            comm = split_oversized(g, comm, resolution);
        }
        densify(&comm)
    }
}

/// Full Leiden (Traag et al. 2019): local move → refinement (well-connected
/// sub-communities) → aggregation BY THE REFINED partition → recurse, until the
/// partition is stable. Deterministic (fixed order, greedy, no RNG). This adds the
/// multi-level aggregation the single-level [`Louvain`] omits, and the refinement
/// that guarantees every community is internally well-connected — both recover real
/// structure on a (kNN-)densified graph.
#[derive(Debug, Default, Clone, Copy)]
pub struct Leiden;

impl CommunityDetector for Leiden {
    fn detect(&self, g: &Graph, resolution: f64) -> Vec<usize> {
        densify(&leiden(g, resolution))
    }
}

/// Generalized one Louvain-style local-move level, starting from `init` (not
/// necessarily singletons) and honouring self-loops via `weighted_degree`. Moves
/// each node to the neighbouring community of maximum modularity gain, fixed order,
/// deterministic tie-break to the smallest community id, until no node moves.
fn local_move(g: &Graph, init: &[usize], resolution: f64) -> Vec<usize> {
    let m = g.total_weight();
    if m <= 0.0 {
        return init.to_vec();
    }
    let two_m = 2.0 * m;
    let k: Vec<f64> = (0..g.n).map(|i| g.weighted_degree(i)).collect();
    let mut comm = init.to_vec();
    let mut sigma_tot: HashMap<usize, f64> = HashMap::new();
    for i in 0..g.n {
        *sigma_tot.entry(comm[i]).or_insert(0.0) += k[i];
    }
    let mut improved = true;
    let mut passes = 0;
    while improved && passes < 50 {
        improved = false;
        passes += 1;
        for i in 0..g.n {
            let ci = comm[i];
            let mut w_to: BTreeMap<usize, f64> = BTreeMap::new();
            for &(j, w) in &g.adj[i] {
                if j != i {
                    *w_to.entry(comm[j]).or_insert(0.0) += w;
                }
            }
            *sigma_tot.get_mut(&ci).unwrap() -= k[i];
            let w_to_ci = w_to.get(&ci).copied().unwrap_or(0.0);
            let mut best_c = ci;
            let mut best_gain =
                w_to_ci - resolution * k[i] * sigma_tot.get(&ci).copied().unwrap_or(0.0) / two_m;
            for (&c, &w_to_c) in &w_to {
                let st = sigma_tot.get(&c).copied().unwrap_or(0.0);
                let gain = w_to_c - resolution * k[i] * st / two_m;
                if gain > best_gain + 1e-12 || (gain > best_gain - 1e-12 && c < best_c) {
                    best_gain = gain;
                    best_c = c;
                }
            }
            comm[i] = best_c;
            *sigma_tot.entry(best_c).or_insert(0.0) += k[i];
            if best_c != ci {
                improved = true;
            }
        }
    }
    comm
}

/// Leiden refinement: within EACH community of `part`, find well-connected
/// sub-communities by local-moving the community's induced subgraph from singletons,
/// then splitting any internally-disconnected piece. Returns a finer partition
/// (`refined` is a strict refinement of `part`), with globally-unique ids.
fn refine(g: &Graph, part: &[usize], resolution: f64) -> Vec<usize> {
    let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, &c) in part.iter().enumerate() {
        members.entry(c).or_default().push(i);
    }
    let mut refined = vec![0usize; g.n];
    let mut next = 0usize;
    for nodes in members.values() {
        let local: HashMap<usize, usize> =
            nodes.iter().enumerate().map(|(li, &o)| (o, li)).collect();
        let mut sub = Graph::new(nodes.len());
        for &u in nodes {
            let ui = local[&u];
            sub.self_w[ui] += g.self_w.get(u).copied().unwrap_or(0.0);
            for &(v, w) in &g.adj[u] {
                if let Some(&vi) = local.get(&v) {
                    if ui < vi {
                        sub.add_edge(ui, vi, w);
                    }
                }
            }
        }
        let init: Vec<usize> = (0..sub.n).collect();
        let sub_part = refine_connected(&sub, &local_move(&sub, &init, resolution));
        let mut remap: HashMap<usize, usize> = HashMap::new();
        for (li, &orig) in nodes.iter().enumerate() {
            let gid = *remap.entry(sub_part[li]).or_insert_with(|| {
                let id = next;
                next += 1;
                id
            });
            refined[orig] = gid;
        }
    }
    refined
}

/// Aggregate `g` by the `refined` partition: each refined sub-community → one node.
/// adj = inter-subcommunity edge weights; self_w = intra-subcommunity edge weight +
/// carried-over self-loops (so degrees stay correct across levels). Returns the new
/// graph, each new node's original-node members, and the LIFTED `part` (the coarse
/// community per refined group → the initial partition for the next level).
fn aggregate(
    g: &Graph,
    node_orig: &[Vec<usize>],
    refined: &[usize],
    part: &[usize],
) -> (Graph, Vec<Vec<usize>>, Vec<usize>) {
    let mut rid: BTreeMap<usize, usize> = BTreeMap::new();
    for &r in refined {
        let next = rid.len();
        rid.entry(r).or_insert(next);
    }
    let mut ag = Graph::new(rid.len());
    let mut ag_node_orig: Vec<Vec<usize>> = vec![Vec::new(); rid.len()];
    let mut ag_part: Vec<usize> = vec![0; rid.len()];
    let mut wmap: HashMap<(usize, usize), f64> = HashMap::new();
    for u in 0..g.n {
        let ru = rid[&refined[u]];
        ag_node_orig[ru].extend_from_slice(&node_orig[u]);
        ag_part[ru] = part[u]; // every node in a refined group shares one coarse community
        ag.self_w[ru] += g.self_w.get(u).copied().unwrap_or(0.0);
        for &(v, w) in &g.adj[u] {
            let rv = rid[&refined[v]];
            if ru == rv {
                ag.self_w[ru] += w / 2.0; // internal edge counted from both endpoints
            } else {
                let key = (ru.min(rv), ru.max(rv));
                *wmap.entry(key).or_insert(0.0) += w; // doubled (both directions)
            }
        }
    }
    for ((a, b), w) in wmap {
        ag.add_edge(a, b, w / 2.0);
    }
    for nb in &mut ag.adj {
        nb.sort_by(|x, y| x.0.cmp(&y.0));
    }
    (ag, ag_node_orig, ag_part)
}

/// Drive full Leiden: local move → refine → aggregate by refined → recurse, until
/// the refined aggregation stops collapsing nodes. Returns the (non-dense) community
/// id per ORIGINAL node.
fn leiden(g0: &Graph, resolution: f64) -> Vec<usize> {
    let mut g = g0.clone();
    let mut node_orig: Vec<Vec<usize>> = (0..g0.n).map(|i| vec![i]).collect();
    let mut init: Vec<usize> = (0..g.n).collect();
    let mut orig_comm: Vec<usize> = (0..g0.n).collect();
    for _level in 0..50 {
        let part = local_move(&g, &init, resolution);
        for (cur, origs) in node_orig.iter().enumerate() {
            for &o in origs {
                orig_comm[o] = part[cur];
            }
        }
        let refined = refine(&g, &part, resolution);
        let (ag, ag_node_orig, ag_part) = aggregate(&g, &node_orig, &refined, &part);
        if ag.n == g.n {
            break; // refinement could not collapse anything → converged
        }
        g = ag;
        node_orig = ag_node_orig;
        init = ag_part;
    }
    orig_comm
}

/// graphify `_MAX_COMMUNITY_FRACTION` — a community covering more than this
/// fraction of all nodes is too coarse to be a topic and is re-clustered.
const MAX_COMMUNITY_FRACTION: f64 = 0.25;
/// graphify `_MIN_SPLIT_SIZE` — never split below this many nodes (the threshold
/// is `max(fraction*n, MIN_SPLIT_SIZE)`), so small graphs aren't over-shredded.
const MIN_SPLIT_SIZE: usize = 10;

/// Recursively split communities larger than `max(25% of nodes, 10)` by
/// re-clustering each oversized community's induced subgraph. Iterates so a
/// still-oversized sub-community is split again; a bounded pass count prevents a
/// pathological dense blob (no modularity structure to split) from looping.
fn split_oversized(g: &Graph, mut comm: Vec<usize>, resolution: f64) -> Vec<usize> {
    let max_size = ((g.n as f64 * MAX_COMMUNITY_FRACTION).floor() as usize).max(MIN_SPLIT_SIZE);
    for _pass in 0..8 {
        let mut members: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (i, &c) in comm.iter().enumerate() {
            members.entry(c).or_default().push(i);
        }
        let mut next_id = comm.iter().copied().max().map_or(0, |m| m + 1);
        let mut changed = false;
        for (_c, nodes) in members {
            if nodes.len() <= max_size {
                continue;
            }
            // induced subgraph over this community's nodes (local indices)
            let local: HashMap<usize, usize> =
                nodes.iter().enumerate().map(|(li, &orig)| (orig, li)).collect();
            let mut sub = Graph::new(nodes.len());
            for &u in &nodes {
                let ui = local[&u];
                for &(v, w) in &g.adj[u] {
                    if let Some(&vi) = local.get(&v) {
                        if ui < vi {
                            sub.add_edge(ui, vi, w);
                        }
                    }
                }
            }
            // re-cluster; if it produced >1 community, adopt the split
            let mut sub_comm = louvain_one_level(&sub, resolution);
            sub_comm = refine_connected(&sub, &sub_comm);
            let distinct: HashSet<usize> = sub_comm.iter().copied().collect();
            if distinct.len() <= 1 {
                continue; // unsplittable blob — leave it (avoids infinite loop)
            }
            let mut remap: HashMap<usize, usize> = HashMap::new();
            for (li, &orig) in nodes.iter().enumerate() {
                let gid = *remap.entry(sub_comm[li]).or_insert_with(|| {
                    let id = next_id;
                    next_id += 1;
                    id
                });
                comm[orig] = gid;
            }
            changed = true;
        }
        if !changed {
            break;
        }
    }
    comm
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
    fn knn_edges_connect_embedding_neighbors() {
        // 4 files with all-distinct entities → from_file_entities = 4 disconnected singletons.
        // Embeddings: {0,1} near each other, {2,3} near each other → kNN must connect the pairs
        // so the detector forms two communities (the singleton fix for related-but-no-shared-entity).
        let fe = vec![hs(&[10]), hs(&[11]), hs(&[12]), hs(&[13])];
        let mut g = Graph::from_file_entities(&fe);
        assert!(g.adj.iter().all(|a| a.is_empty()), "no shared entities → no edges before kNN");
        let emb = vec![
            vec![1.0, 0.0], vec![0.98, 0.05], // cluster A
            vec![0.0, 1.0], vec![0.05, 0.98], // cluster B
        ];
        g.add_knn_edges(&emb, 1, 1.0);
        let comm = Louvain { leiden_refine: true }.detect(&g, 1.0);
        assert_eq!(comm[0], comm[1], "embedding-near files 0,1 must cluster");
        assert_eq!(comm[2], comm[3], "embedding-near files 2,3 must cluster");
        assert_ne!(comm[0], comm[2], "the two embedding clusters stay distinct");
    }

    fn each_community_connected(g: &Graph, comm: &[usize]) -> bool {
        let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, &c) in comm.iter().enumerate() {
            members.entry(c).or_default().push(i);
        }
        for nodes in members.values() {
            let set: HashSet<usize> = nodes.iter().copied().collect();
            let mut seen = HashSet::new();
            let mut q = VecDeque::new();
            q.push_back(nodes[0]);
            seen.insert(nodes[0]);
            while let Some(u) = q.pop_front() {
                for &(v, _) in &g.adj[u] {
                    if set.contains(&v) && seen.insert(v) {
                        q.push_back(v);
                    }
                }
            }
            if seen.len() != nodes.len() {
                return false;
            }
        }
        true
    }

    #[test]
    fn leiden_two_clusters_separate() {
        let fe = vec![hs(&[100]), hs(&[100]), hs(&[100]), hs(&[200]), hs(&[200]), hs(&[200])];
        let g = Graph::from_file_entities(&fe);
        let comm = Leiden.detect(&g, 1.0);
        assert_eq!(comm[0], comm[1]);
        assert_eq!(comm[1], comm[2]);
        assert_eq!(comm[3], comm[4]);
        assert_eq!(comm[4], comm[5]);
        assert_ne!(comm[0], comm[3]);
    }

    #[test]
    fn leiden_isolated_nodes_each_own_community() {
        let fe = vec![hs(&[1]), hs(&[2]), hs(&[3])];
        let g = Graph::from_file_entities(&fe);
        assert_eq!(
            Leiden.detect(&g, 1.0).iter().copied().collect::<HashSet<_>>().len(),
            3
        );
    }

    #[test]
    fn leiden_deterministic() {
        let fe = vec![
            hs(&[1, 2]), hs(&[2, 3]), hs(&[3, 1]), hs(&[9]), hs(&[9]),
            hs(&[5, 6]), hs(&[6, 7]), hs(&[7, 5]),
        ];
        let g = Graph::from_file_entities(&fe);
        assert_eq!(Leiden.detect(&g, 1.0), Leiden.detect(&g, 1.0));
    }

    #[test]
    fn leiden_modularity_at_least_louvain() {
        // two tight triangles, weakly bridged — multi-level Leiden must not do worse.
        let fe = vec![
            hs(&[1, 2]), hs(&[2, 3]), hs(&[3, 1]),
            hs(&[5, 6]), hs(&[6, 7]), hs(&[7, 5]),
        ];
        let g = Graph::from_file_entities(&fe);
        let ql = modularity(&g, &Louvain { leiden_refine: true }.detect(&g, 1.0), 1.0);
        let qle = modularity(&g, &Leiden.detect(&g, 1.0), 1.0);
        assert!(qle >= ql - 1e-9, "leiden modularity {qle} < louvain {ql}");
    }

    #[test]
    fn leiden_communities_internally_connected() {
        let fe = vec![
            hs(&[1, 2]), hs(&[2, 3]), hs(&[3, 1]),
            hs(&[5, 6]), hs(&[6, 7]), hs(&[7, 5]), hs(&[9]),
        ];
        let g = Graph::from_file_entities(&fe);
        let comm = Leiden.detect(&g, 1.0);
        assert!(each_community_connected(&g, &comm), "every Leiden community must be connected");
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
    fn oversized_clique_not_shattered() {
        // Two 12-node cliques (each via a shared entity) exceed max_size(=10), so
        // the graphify-style oversized splitter runs — but a clique has no
        // sub-structure, so it must NOT be shattered (over-shred guard), and the
        // two cliques must stay distinct. Also checks determinism with split on.
        let mut fe = Vec::new();
        for _ in 0..12 {
            fe.push(hs(&[100])); // group A: files 0..=11
        }
        for _ in 0..12 {
            fe.push(hs(&[200])); // group B: files 12..=23
        }
        fe[0].insert(300); // one weak bridge between the two cliques
        fe[12].insert(300);
        let g = Graph::from_file_entities(&fe);
        let comm = Louvain { leiden_refine: true }.detect(&g, 1.0);
        for i in 1..12 {
            assert_eq!(comm[0], comm[i], "A node {i} split off");
        }
        for i in 13..24 {
            assert_eq!(comm[12], comm[i], "B node {i} split off");
        }
        assert_ne!(comm[0], comm[12], "cliques merged");
        assert_eq!(comm, Louvain { leiden_refine: true }.detect(&g, 1.0));
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
