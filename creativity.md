# The Novelty Playbook: Creative Thinking Models for Generating Novel Ideas and Hypotheses (A Foundational Skill for NoveltyBench Agents)

## TL;DR
- **Novelty is an engineering discipline, not a muse.** The most reliable engine of genuinely new ideas is *structural transfer* — importing a solution pattern (a mechanism) from a source domain into a target domain by analogy (GANs from game theory, simulated annealing from metallurgy, neural nets from the brain), combined with *first-principles decomposition* (strip a problem to its irreducible truths and rebuild) and *bisociation/conceptual blending* (forcibly combining two unrelated frames). This document catalogs all 39 thinking skills in the cc-thinking-skills repo, distills the science of creativity, extracts the 10 principles of *Steal Like an Artist*, and fuses them into an operational playbook.
- **To win NoveltyBench-style benchmarks you must beat mode collapse.** The original NoveltyBench (Zhang et al., CMU, arXiv 2504.05228) scores `distinct_k` (count of functionally distinct outputs in k samples) and `utility_k` (novelty × quality). The evo-hq autoresearch scaffold rewards **mechanism-distinct candidates that anticipate future breakthroughs** (a candidate matching the snapshot's "future-improvement pool" scores +impact×confidence; one matching an already-tried prior scores negative), plus a diversity bonus equal to mean pairwise cosine distance between your N candidates. The dominant strategy is therefore: maximize *mechanistic* spread across candidates, ground each in real literature/structure, and avoid both rediscovery of failed priors and near-duplicate paraphrases.
- **The operational loop:** (1) decompose with first principles → (2) reframe the problem → (3) generate via cross-domain analogy + remote association + blending → (4) force diversity with constraints, inversion, random stimulus, and SCAMPER/TRIZ → (5) form hypotheses abductively from anomalies → (6) diverge widely, then converge with calibrated evaluation. Spread candidates across *different generating mechanisms*, not surface paraphrases.

---

## Key Findings

1. **The cc-thinking-skills repo contains 39 thinking skills** (not 18, as the GitHub "About" blurb stale-claims) organized into seven families: Decision Making & Analysis, Cognitive & Behavioral, Systems & Strategy, Problem Solving & Innovation, Estimation & Risk, Product & Innovation, and Meta-Skills. The meta-skill `thinking-model-router` is the designated entry point.
2. **Human creativity is mechanistically a search-and-recombination process** governed by the interplay of two brain networks: the Default Mode Network (spontaneous idea generation) and the Executive Control Network (evaluation/constraint). Creative people have *flatter associative hierarchies* (Mednick) — they reach remote, low-probability associations rather than the dominant obvious one. This is exactly the cognitive profile a mode-collapsed LLM lacks.
3. **Cross-domain analogical transfer is the single most documented engine of breakthrough innovation.** GANs (game theory → ML), simulated annealing (thermodynamics → optimization), genetic algorithms (Darwinian evolution → search), neural networks (neuroscience → computing), PageRank (citation analysis → web search), Velcro (burdock burrs → fasteners), and Kekulé's benzene ring (a dream of a snake → chemistry) all instantiate the same move: map relational structure from a source domain onto a target.
4. **First-principles thinking and analogical thinking are complementary, not opposed.** First principles decomposes a problem to physical/logical bedrock to escape inherited assumptions; analogy then *imports a mechanism* to fill the solution. Use first principles to find what is genuinely required; use analogy to find a novel way to deliver it.
5. **Scientific hypotheses are generated abductively** (Peirce): a surprising anomaly is observed, and one reasons backward to the explanation that, if true, would render the anomaly a matter of course. Anomalies and gaps are the richest raw material for novel hypotheses.

---

## Details

### Part A — The cc-thinking-skills Repository: Complete Catalog of 39 Thinking Models

The repository (`tjboudreaux/cc-thinking-skills`, MIT-licensed) packages 39 mental models as Claude Code "skills," each a `SKILL.md` with a process, examples, a template, and a verification checklist. Invoke a skill by name (e.g., "Use first-principles thinking to analyze this architecture decision"). Below is every skill, what it does, its mechanism, and a real-world use case.

**Decision Making & Analysis**
1. **thinking-first-principles** — Break problems into fundamental truths and rebuild from basics (Aristotle/Musk). *Mechanism:* list assumptions → challenge each ("is this physics or convention?") → reduce to irreducible elements → reason up. *Use case:* a startup told "battery packs cost $600/kWh" decomposes to raw-material cost (~$80/kWh) and concludes the constraint is manufacturing, not physics.
2. **thinking-second-order** — Think beyond immediate consequences ("and then what?"). *Mechanism:* trace 2nd/3rd-order effects of a decision over time. *Use case:* pricing change that lifts revenue now but trains customers to wait for discounts.
3. **thinking-inversion** — Approach the goal by identifying paths to failure. *Mechanism:* ask "how would we guarantee the worst outcome?" then avoid those. *Use case:* instead of "how do we retain users," ask "what would make everyone quit?"
4. **thinking-pre-mortem** — Imagine the project has already failed and work backward. *Mechanism:* assume failure, brainstorm causes, mitigate in advance. *Use case:* project kickoff risk assessment.
5. **thinking-kepner-tregoe** — Systematic rational process for complex problem analysis. *Mechanism:* situation appraisal → problem analysis → decision analysis → potential-problem analysis. *Use case:* high-stakes root-cause investigation in manufacturing.
6. **thinking-reversibility** — Classify decisions as Type 1 (irreversible) vs Type 2 (reversible). *Mechanism:* match decision speed to reversibility. *Use case:* deciding whether a one-way-door architecture commitment deserves slow deliberation.
7. **thinking-regret-minimization** — Project to your future self to test a decision (Bezos). *Mechanism:* imagine yourself at 80 looking back. *Use case:* whether to leave a stable job to start a company.
8. **thinking-opportunity-cost** — Evaluate choices by what you give up. *Mechanism:* make the implicit alternative explicit. *Use case:* allocating an engineer to feature A vs B.

**Cognitive & Behavioral**
9. **thinking-bayesian** — Update beliefs based on evidence. *Mechanism:* prior × likelihood → posterior. *Use case:* interpreting a positive result from an imperfect diagnostic test.
10. **thinking-debiasing** — Identify and counteract cognitive biases. *Mechanism:* checklist of biases (anchoring, confirmation) + countermeasures. *Use case:* de-biasing a hiring panel.
11. **thinking-dual-process** — Recognize when to trust intuition (System 1) vs analysis (System 2). *Mechanism:* match mode to stakes/speed. *Use case:* fast incident triage vs slow architectural choice.
12. **thinking-bounded-rationality** — Make good-enough (satisficing) decisions under constraints. *Mechanism:* set an aspiration threshold, stop searching when met. *Use case:* vendor selection under time pressure.
13. **thinking-socratic** — Systematic questioning framework. *Mechanism:* iterative "why/how do you know" probing. *Use case:* clarifying vague product requirements.
14. **thinking-probabilistic** — Calibrated probability estimation. *Mechanism:* express beliefs as probabilities, track calibration. *Use case:* forecasting a launch date as a distribution.
15. **thinking-steel-manning** — Argue the strongest version of the opposing position. *Mechanism:* improve the opponent's argument before rebutting. *Use case:* validating a strategy decision against the best counterargument.

**Systems & Strategy**
16. **thinking-systems** — Analyze interconnected wholes with feedback loops and emergence. *Mechanism:* map stocks, flows, loops. *Use case:* debugging emergent behavior in a distributed system.
17. **thinking-feedback-loops** — Identify reinforcing and balancing loops. *Mechanism:* classify loops, find which dominates. *Use case:* designing a viral growth mechanism.
18. **thinking-archetypes** — Recognize recurring system patterns (e.g., "tragedy of the commons"). *Mechanism:* match situation to a known archetype. *Use case:* diagnosing why a shared service keeps degrading.
19. **thinking-ooda** — Observe–Orient–Decide–Act loop for dynamic situations (Boyd). *Mechanism:* cycle rapidly, outpace the adversary. *Use case:* live incident response or competitive response.
20. **thinking-leverage-points** — Find where small changes have big effects (Meadows). *Mechanism:* rank intervention points by leverage. *Use case:* choosing the one metric that moves the system.
21. **thinking-theory-of-constraints** — Every system has exactly one binding constraint (Goldratt). *Mechanism:* identify bottleneck → exploit → subordinate → elevate. *Use case:* throughput optimization in a CI/CD pipeline.
22. **thinking-cynefin** — Classify problems as Clear, Complicated, Complex, or Chaotic. *Mechanism:* match approach to domain. *Use case:* deciding whether to use best practice vs experimentation.

**Problem Solving & Innovation**
23. **thinking-occams-razor** — Prefer the simplest adequate explanation. *Mechanism:* minimize assumptions. *Use case:* debugging — check the common cause before the exotic one.
24. **thinking-map-territory** — Recognize that the model is not reality. *Mechanism:* flag where the abstraction breaks. *Use case:* when metrics diverge from user-reported reality.
25. **thinking-circle-of-competence** — Know the boundaries of your expertise. *Mechanism:* mark inside/edge/outside. *Use case:* deciding what to delegate vs learn.
26. **thinking-triz** — Resolve technical contradictions with 40 inventive principles (Altshuller). *Mechanism:* express the trade-off as a contradiction, look up applicable inventive principles. *Use case:* an engineering design where making the part stronger makes it heavier.
27. **thinking-five-whys-plus** — Enhanced root-cause analysis with bias guards. *Mechanism:* iterate "why" with checks against premature closure. *Use case:* incident postmortem.
28. **thinking-scientific-method** — Hypothesis-driven investigation. *Mechanism:* hypothesis → prediction → experiment → revise. *Use case:* A/B testing a feature.
29. **thinking-thought-experiment** — Structured imagination for exploring edge cases. *Mechanism:* construct a hypothetical, reason through consequences. *Use case:* stress-testing an architecture against a 100× load scenario.

**Estimation & Risk**
30. **thinking-fermi-estimation** — Order-of-magnitude calculations from decomposition. *Mechanism:* break unknown into estimable factors. *Use case:* sizing a market or a compute bill.
31. **thinking-margin-of-safety** — Build buffers for uncertainty. *Mechanism:* design to tolerate worse-than-expected. *Use case:* capacity planning.
32. **thinking-lindy-effect** — Older non-perishables are likely to last longer. *Mechanism:* use age as a survival predictor. *Use case:* choosing a durable technology stack.
33. **thinking-via-negativa** — Improve by removing, not adding. *Mechanism:* subtract sources of fragility. *Use case:* simplifying a bloated codebase.
34. **thinking-red-team** — Attack your own plan adversarially. *Mechanism:* assign an adversary role to find weaknesses. *Use case:* pre-launch security and plan review.

**Product & Innovation**
35. **thinking-jobs-to-be-done** — Understand the "job" customers hire a product for. *Mechanism:* identify the underlying job, not the feature. *Use case:* the "milkshake hired for a boring commute" insight.
36. **thinking-effectuation** — Start with available means, not predetermined goals. *Mechanism:* bird-in-hand, affordable loss, leverage contingency. *Use case:* a founder building from existing skills/network under uncertainty.

**Meta-Skills**
37. **thinking-model-router** — *START HERE.* Routes you to the right model by domain. *Use case:* entry point when you don't know which model to use.
38. **thinking-model-selection** — Choose the right model for a new problem. *Use case:* approach selection.
39. **thinking-model-combination** — Combine multiple models for richer analysis. *Use case:* complex, high-stakes decisions needing several lenses.

**Note for NoveltyBench:** the highest-value skills for *novelty generation* are first-principles, inversion, thought-experiment, TRIZ, scientific-method, and the combination meta-skill. The decision/risk skills are mostly *convergent* (for evaluating and pruning candidates), which matters for the "quality" half of utility scoring.

### Part B — The Science of Creativity and Novel Idea Generation

**Divergent vs. convergent thinking (Guilford, 1956/1967).** J.P. Guilford distinguished *divergent thinking* — generating many varied solutions to an open-ended problem, measured by fluency, flexibility, originality, and elaboration — from *convergent thinking*, which narrows to the single correct answer. Creativity requires both, applied at the right phase: diverge to generate, converge to select. Guilford's Alternative Uses Test (uses for a brick/paperclip) operationalized divergent thinking. **Implication for agents:** mode-collapsed LLMs are over-trained for convergence (the single most-likely answer); novelty requires deliberately forcing the divergent phase.

**Associative theory and remote associates (Mednick, 1962).** Mednick defined the creative process as "the forming of associative elements into new combinations which... are in some way useful. The more mutually remote the elements of the new combination, the more creative." Creative people have *flat associative hierarchies* — given "table," they readily retrieve remote responses ("leg," "food") rather than being stuck on the dominant one ("chair"). His Remote Associates Test (RAT) gives three words (e.g., "rat–blue–cottage") requiring a fourth linking word ("cheese"). **Implication:** novelty = reaching the long tail of the association distribution. A diversity-maximizing agent should deliberately sample remote, low-probability associations.

**Bisociation (Koestler, *The Act of Creation*, 1964).** Koestler argued every creative act is the *bisociation* of two self-consistent but habitually incompatible "matrices" (frames of reference). "The creative act moves seamlessly from the 'Aha!' of scientific discovery to the 'Ah…' of aesthetic insight to the 'Ha-ha' of the pun." A pun ("two strings of thought tied together by an acoustic knot") forces the mind to hold two frames at once; so does a scientific discovery that fuses two previously separate domains (e.g., physics + chemistry). **This is the theoretical backbone of cross-domain transfer:** force a collision between two unrelated frames and look at the link.

**Conceptual blending (Fauconnier & Turner, 1990s–2002).** Blending theory describes how we build novel meaning by projecting elements from *two input mental spaces* into a *blended space* that develops *emergent structure* of its own — structure present in neither input. The network has four spaces: two inputs, a *generic space* (what they share), and the *blend*. Famous example: the "clipper ship race against its own past voyage." **Implication:** to invent, take two concepts, find the shared generic structure, project selectively, and run the blend to see what emerges.

**Analogy and structure-mapping (Gentner, 1983).** Analogy is the *mapping of relational structure* from a base/source domain to a target, with a *systematicity bias*: people prefer to map connected systems of higher-order (causal, mathematical) relations rather than isolated surface features. Gentner's canonical case: Rutherford's atom-as-solar-system maps the *relations* (a small body orbits a large central one because of an attractive force) not the surface attributes (the sun is hot/yellow). The Structure-Mapping Engine (Falkenhainer, Forbus & Gentner, 1989) computationalized this. **Implication:** good novelty transfers *deep relational structure*, not superficial resemblance.

**Geneplore model (Finke, Ward & Smith, 1992).** "Geneplore" = *generate* + *explore*. Creative cognition has two phases: a **generative phase** producing "preinventive structures" (candidate ideas) via retrieval, association, mental synthesis, mental transformation, **analogical transfer**, and categorical reduction; and an **exploratory phase** that interprets, tests, and elaborates them (hypothesis testing, attribute finding, functional inference, contextual shifting). Crucially, *generation should be less constrained than exploration* — produce wide, weird preinventive structures first, judge later. **This maps directly onto the diverge-then-converge loop and onto NoveltyBench's "produce many, then evaluate" structure.**

**Default Mode Network ↔ Executive Control Network interplay.** Creativity neuroscience (Beaty, Benedek, Kounios, Beeman, and others) consistently finds that creative cognition recruits the **Default Mode Network (DMN)** — associated with spontaneous, internally-directed, associative thought (idea generation) — coupled with the **Executive Control Network (ECN)** — associated with focused, goal-directed evaluation (idea selection/constraint). These networks usually act in opposition; in creative thinking they cooperate, with the DMN generating candidates and the ECN evaluating and shaping them to task constraints. In the largest and most ethnically diverse creativity-neuroscience study to date — Chen, Kenett, Cui, Beaty et al., "Dynamic switching between brain networks predicts creative ability," *Communications Biology* 8, Article 54 (2025), analyzing resting-state fMRI and creative-task performance across 10 independent samples from Austria, Canada, China, Japan, and the United States (N = 2,433) — the authors found that "creativity, but not general intelligence, can be reliably predicted by the number of DMN–ECN switches," with an inverted-U relationship validated by an independent task-fMRI sample (N = 31). **Implication:** a good creative *architecture* literally separates a generative subsystem from an evaluative one — exactly the Geneplore and diverge/converge structure.

**Insight, incubation, and the "Aha!" moment (Kounios & Beeman).** EEG/fMRI studies show insight solutions are preceded by *unconscious processing*. Jung-Beeman, Bowden, Kounios et al. (2004, *PLOS Biology*) found "increased activity in the right hemisphere anterior superior temporal gyrus for insight relative to noninsight solutions," and EEG revealed "a sudden burst of high-frequency (gamma-band) neural activity in the same area beginning 0.3 s prior to insight solutions" — a region tied to making distant semantic connections (metaphors, jokes, gist), often preceded by an alpha-band "brain blink" that gates out distraction. Incubation (stepping away) lets unconscious recombination proceed. **Implication for agents:** "incubation" can be simulated by deliberately changing representation/context between generation passes (re-prompting from a different frame), which surfaces remote associations the first pass suppressed.

**Combinatorial creativity / BVSR.** Across all these theories runs one idea: novelty is *recombination*. Simonton's Blind-Variation-and-Selective-Retention (BVSR) frames creativity as generating many variants (some "blind"/non-obvious) and selectively retaining the good ones — an evolutionary search. This is the same shape as Geneplore and as the NoveltyBench propose-many-then-score loop.

### Part C — *Steal Like an Artist* (Austin Kleon, 2012): The 10 Principles

Kleon's thesis is that creativity is recombination of influences, not creation ex nihilo — directly aligned with combinatorial/associative theory.

1. **Steal like an artist.** "Nothing is original. All creative work builds on what came before." (Cf. Ecclesiastes "nothing new under the sun"; T.S. Eliot: "good poets... make it into something better, or at least something different.") *Idea-gen relevance:* every new idea is "a mashup or a remix of one or more previous ideas." Collect good ideas voraciously; a "swipe file" is your raw material library. **1 + 1 = 3** — combining two influences produces a third thing in the negative space between them.
2. **Don't wait until you know who you are to get started.** Identity emerges from making. "Fake it 'til you make it" — start producing before you feel ready; the doing reveals the direction.
3. **Write the book you want to read.** "The manifesto is this: ...do the work you want to see done." Generate by identifying the gap — what your heroes *didn't* make. (This is gap-driven ideation.)
4. **Use your hands.** Move between the analog and digital; bodily engagement and tactility kickstart thinking ("our bodies can tell our brains as much as our brains tell our bodies"). The computer "brings out the uptight perfectionist... we start editing ideas before we have them" — separate generation from editing.
5. **Side projects and hobbies are important.** "The work you do while you procrastinate is probably the work you should be doing." Cross-pollination between passions ("let them talk to each other") is where novelty originates — the Medici-Effect intersection at personal scale.
6. **The secret: do good work and share it with people.** Share process, not just product; the network returns ideas and feedback. "Be open... you can put yourself online to find something to say."
7. **Geography is no longer our master.** You can build a creative community anywhere; but "distance and difference are the secret tonic of creativity" — exposure to unfamiliar contexts makes the brain work harder.
8. **Be nice. (The world is a small town.)** Surround yourself with the best — "garbage in, garbage out" applied to idea inputs. Channel even anger/curiosity into making.
9. **Be boring. (It's the only way to get work done.)** Creativity needs routine, energy, constraints of discipline. Volume and consistency (Seinfeld's "don't break the chain") beat waiting for inspiration.
10. **Creativity is subtraction.** "In this age of information abundance... those who get ahead will be the folks who figure out what to leave out." *Constraints drive creativity:* Dr. Seuss wrote *Green Eggs and Ham* on a 50-word bet. "Telling yourself you have all the time, all the money, all the colors... that just kills creativity" (Jack White). **Choosing what to leave out is itself a generative act.**

### Part D — Cross-Domain Analogy as the Engine of Innovation (with Many Examples)

The recurring pattern of breakthrough: **innovation = transferring a solution pattern (mechanism/relational structure) from a source domain into a target domain via structural analogy** (Gentner's structure-mapping made concrete). The source is often an *adjacent or distant* discipline.

- **Generative Adversarial Networks ← game theory.** Ian Goodfellow et al. (2014) framed generative modeling as a **two-player minimax (zero-sum) game** between a *generator* (minimizing) and a *discriminator* (maximizing), seeking a Nash-style equilibrium where synthetic data is indistinguishable from real. The imported structure is the *adversarial minimax game* from economics/mathematics — not a surface feature but the core relational mechanism.
- **Simulated annealing ← metallurgy/thermodynamics.** Kirkpatrick et al. (1983) mapped the physical *annealing* process (heat a metal, cool slowly so atoms settle into a minimal-energy crystal) onto combinatorial optimization: a "temperature" parameter controls the probability of accepting worse solutions, decreasing on a cooling schedule to escape local optima. The Metropolis acceptance criterion is the literal physics equation repurposed.
- **Genetic algorithms / evolutionary computation ← Darwinian evolution.** Selection, crossover, and mutation over a population of candidate solutions — natural selection imported as a search algorithm.
- **Artificial neural networks ← neuroscience.** Neurons, weighted synapses, and activation thresholds modeled on the brain.
- **Reinforcement learning ← behavioral psychology.** Reward signals, value, and policy learning map onto operant conditioning (Thorndike's law of effect, Skinner) — reward-driven trial-and-error.
- **Ant colony optimization ← insect foraging.** Dorigo (1992) mapped ants' pheromone-trail-laying onto pathfinding: artificial "pheromone" reinforces good paths in a graph. Particle swarm optimization similarly imports flocking/schooling.
- **PageRank ← academic citation analysis (bibliometrics).** Treat a hyperlink as a citation; a page is important if important pages link to it — eigenvector centrality borrowed from citation networks/sociometry.
- **Velcro ← burdock burrs (biomimicry).** George de Mestral (1941) examined burrs stuck to his dog under a microscope, saw the hook-and-loop mechanism, and reproduced its *physics* (not its appearance) as a fastener.
- **Kekulé's benzene ring ← a dream of a snake biting its tail (the ouroboros).** The hexagonal ring structure of benzene reportedly came to August Kekulé via a reverie of a snake seizing its own tail — a visual-symbolic blend imported into molecular structure.
- **Darwin's natural selection ← Malthus's economics of population.** Reading Malthus's *Essay on the Principle of Population* (geometric population growth against limited resources) gave Darwin the mechanism — competition and differential survival — for biological evolution.
- **The Wright brothers ← birds + bicycle mechanics.** They studied wing-warping in soaring birds for roll control and applied bicycle-balance intuitions (an unstable craft controlled by the rider) to flight control.
- **Bayesian inference ← cross-field portability.** A single mathematical structure (update priors with evidence) transferred across domains: spam filtering, medical diagnosis, search-and-rescue (locating a lost submarine), and ML.

The general operation in each: identify the *deep relational structure* of a known solution in domain A; find a target problem in domain B with the same abstract structure; map and adapt. **This is the most reliable, most teachable route to non-obvious novelty — and the one most useful for a NoveltyBench agent, because it produces ideas that are genuinely mechanistically distinct rather than paraphrases.**

### Part E — First-Principles Decomposition

**Definition and lineage.** A *first principle* is a foundational truth that cannot be deduced from anything more basic (Aristotle, *Posterior Analytics*: "the first basis from which a thing is known"). Descartes radicalized it with systematic doubt. Musk repopularized it: "boil things down to the most fundamental truths... and then reason up from there," explicitly contrasting it with reasoning *by analogy* ("copying what other people do with slight variations").

**The contrast and complementarity with analogy.** Reasoning by analogy is fast, cheap, and incremental — and is *also* (per Part D) a powerful novelty engine when the analogy crosses domains. First-principles reasoning is slow and effortful but escapes inherited constraints. **They combine best as a two-stroke engine:** use first principles to determine *what is actually, physically required* (stripping away conventional "truths" that are really just historical artifacts); then use cross-domain analogy to *import a novel mechanism* that delivers that requirement.

**A concrete method (decomposition → reconstruction):**
1. **State the problem and surface every assumption.** Write down the conventional wisdom and "best practices." 
2. **Interrogate each assumption:** "Is this a law of nature/logic, or a convention, tradition, or historical accident?" Keep only what is irreducibly true.
3. **Reduce to fundamentals:** the physical quantities, costs, constraints, and goals that remain (e.g., raw-material cost; the actual *function* required, ignoring current *form*).
4. **Reconstruct upward:** rebuild a solution using only the verified fundamentals, now free to recombine them in ways convention forbids.
5. **Re-introduce analogy deliberately:** ask "what mechanism from another domain could satisfy these fundamentals?"

*Worked example (Tesla battery):* industry priced packs at ~$600/kWh as a fixed fact. First-principles decomposition into commodity inputs (lithium, nickel, cobalt, manganese, graphite, steel can, electrolyte) priced at ~$80/kWh revealed the gap was manufacturing/supply-chain, not physics — motivating the Gigafactory strategy.

### Part F — Other Established Ideation Frameworks

- **TRIZ (Altshuller).** Beginning in 1946, Genrich Altshuller analyzed a very large patent corpus (figures cited across sources range from ~40,000 patent abstracts to 200,000+ and, in later accounts, 400,000+) and concluded that breakthrough inventions resolve a **contradiction** (improving one parameter worsens another) rather than trading off. The **Contradiction Matrix** (39 engineering parameters × 39) points to a subset of **40 Inventive Principles** (e.g., Segmentation, Taking-out, Local Quality, Asymmetry, Nesting, "the other way round"/inversion, prior action, the principle of "ideality"). TRIZ has since been adopted by firms including Samsung, GE, BAE Systems, and Mars. *Use:* express your problem as a contradiction, look up the recommended principles, instantiate them.
- **SCAMPER (Osborn/Eberle).** A checklist of transformations applied to an existing idea: **S**ubstitute, **C**ombine, **A**dapt, **M**odify/Magnify, **P**ut to other use, **E**liminate, **R**everse. A fast, mechanical divergence generator.
- **Lateral thinking (de Bono).** Deliberately break out of the dominant pattern; tools include **random entry/random word** (inject an unrelated stimulus and force a connection — operationalizing remote association), provocation ("po"), and challenging assumptions. De Bono demonstrated creativity is a *teachable skill*, not innate genius.
- **Synectics (Gordon & Prince).** "Make the strange familiar and the familiar strange" via four analogy types: **direct** (cross-domain, e.g., biology), **personal** (become the object), **symbolic** (compressed/poetic), and **fantasy** analogy. Explicitly an analogy-engine.
- **Morphological analysis (Zwicky box).** Decompose a design into independent parameters/dimensions, list options for each, and systematically combine across the multidimensional matrix — exhaustive combinatorial generation.
- **Design thinking.** Empathize → Define → Ideate → Prototype → Test; front-loads problem reframing and divergent ideation, then converges via prototyping.
- **Munger's latticework of mental models.** Carry models from *many* disciplines (psychology, physics, biology, economics) and overlay them on a problem; novelty arises where models from different fields intersect — the personal version of the Medici Effect.
- **The Medici Effect (Johansson).** Breakthroughs concentrate at the **Intersection** where disciplines, cultures, and fields collide; maximize the chance of intersectional ideas by deliberately diversifying inputs and combining distant concepts. (Directional ideas refine within a field; Intersectional ideas leap between them.)
- **Constraints as a driver.** Limitations force novelty (Kleon's "creativity is subtraction"; Stravinsky, Dr. Seuss). Adding an artificial constraint is a reliable way to escape the obvious answer.
- **Random stimulus / random entry.** Introduce a random word, image, or concept and force-fit a connection to the problem — a direct mechanical way to reach remote associations and provoke bisociation.
- **Abductive reasoning (see Part G), counterfactual thinking** ("what if X had been different?"), and **inversion** ("solve the opposite problem / avoid failure") round out the toolkit.

### Part G — How Novel Scientific Hypotheses Are Generated

**Abduction (Peirce): the logic of discovery.** Peirce identified a third inference mode beyond deduction and induction: **abduction** ("the process of forming an explanatory hypothesis... the only logical operation which introduces any new idea"). Its canonical form: *A surprising fact C is observed; but if hypothesis A were true, C would be a matter of course; hence there is reason to suspect A is true.* Abduction *generates* candidate explanations; deduction derives their testable consequences; induction tests them against data. Modern usage often equates abduction with **inference to the best explanation (IBE)**, though strictly Peirce's abduction is just the *hypothesis-generative* step.

**Discovery vs. justification.** Philosophy of science distinguishes the *context of discovery* (how a hypothesis is dreamed up — often abductive, analogical, intuitive, "very little hampered by rules of logic") from the *context of justification* (how it is tested). Novelty lives in discovery; rigor lives in justification. A NoveltyBench agent must excel at the *former* while keeping the latter as a quality filter.

**Anomaly/surprise-driven generation.** The richest trigger for a novel hypothesis is a *surprising anomaly* — an observation that violates the current model. Hypothesis generation then asks: "What would have to be true for this anomaly to be expected?" 

**Analogy in hypothesis formation.** Many landmark hypotheses are analogical imports (Part D): Kepler's "why do outer planets move slower?" led him to posit a sun-emanating force by analogy to light/magnetism. The mechanism is structure-mapping applied to explanation.

**Gap-finding.** Researchers pose new questions by mapping the literature and locating *gaps* (unexplained results, untested combinations, assumptions never questioned, methods never transferred across subfields). "Importing method M from subfield X into subfield Y" is a perennial source of novel, validatable hypotheses — exactly the kind of mechanism-distinct proposal the evo-hq benchmark rewards.

### Part H — The NoveltyBench Target: What the Benchmarks Actually Reward

**Original NoveltyBench (Zhang et al., CMU; arXiv 2504.05228).** A benchmark of **1,100 prompts** (NB-Curated: 100 hand-crafted prompts across Randomness, Factual Knowledge, Creative Writing, Subjectivity, each with 8 human responses; NB-WildChat: 1,000 real ChatGPT prompts) designed to elicit *variable* answers. It targets **mode collapse** — the failure to produce diverse outputs. Method: partition a model's k generations into **functional equivalence classes** (two generations are "different" if a user would benefit from seeing both), then score:
- **distinct_k** — the number of functionally distinct equivalence classes among k samples (pure diversity).
- **utility_k** — a unified novelty-and-quality measure: cumulative utility to a user who only benefits from a *new* (distinct) generation, with patience discount p=0.8, counting only novel generations weighted by quality.

Key finding: the authors "evaluate NoveltyBench on twenty frontier models, including GPT-4o, Claude 3.5 Sonnet and Gemini 2.0 Pro, and find that all suffer from a lack of diversity" — models like Claude 3 and GPT-4o "produce on average fewer than 4 distinct responses in 10 queries," and "larger and more capable models in the same model family tend to produce less diverse outputs." Notably, smaller models such as Gemma 2-2B and Llama 3.2-1B demonstrate the *highest* creativity on the benchmark — a direct warning that raw capability fights against novelty.

**The evo-hq autoresearch-novelty-bench scaffold.** This applies the novelty paradigm to **autonomous research ideation**, built on Prime Intellect's ML "speedrunning" archive (agents racing to improve a `modded-nanogpt` recipe toward val_loss ≤ 3.28). The scaffold renders a near-blank-slate workspace giving the **proposer agent** only: the **goal**, the **wave constraint** (the wave's gating rule), the **field-wide best step count** at that wall-clock moment, and the **current-best `variant.py`** — then asks it to write **N mechanism-distinct candidate proposals** as markdown files in `scratchpad/ideas/`. The agent must use its *own* tools (web search, paper retrieval, training knowledge) to gather context.

**How the judge scores (the rules you must optimize for):** `nb.score()` (default backend `llm-hybrid`: `text-embedding-3-large` for retrieval + `gpt-5-mini` reasoning to classify each candidate) computes a **set score**:
```
set_score = sum(per_candidate_scores)
          + 0.5 × diversity_bonus   # mean pairwise cosine distance between your N candidates
          + 0.1 × validity_term     # fraction of candidates passing the structural check
```
Each candidate is classified, in priority order:
1. **invalid → −1.0** (missing `## Proposal` section, no title, or body < 50 chars).
2. **novel_validated → +impact × confidence** — the candidate semantically matches an experiment in the snapshot's **future-improvement pool** (something other researchers proved out *later*; i.e., you *anticipated* a winning thread). Impact tiers: **1.0 frontier_idea, 0.6 improved_idea, 0.5 frontier_experiment, 0.4 improved_experiment.**
3. **rediscovery → −0.5 × rejection_mult × confidence** — matches a *prior* (already tried before this moment). Rejection multipliers escalate with how decisively the prior was killed: none 1.0, failed 1.4, family_ruled_out 1.6, audit_noncompliant 1.6, **existence_killed 2.0.**
4. **novel_unvalidated → +0.3 × confidence** — no prior match and no future match (genuinely new but unverified).

A crucial rule: **"future-first priority — copying a prior that ended up on a winning thread counts as anticipation, not rediscovery."** Distinctness is enforced two ways: the **diversity_bonus** (near-duplicate candidates have low pairwise cosine distance and earn little bonus) and the **mechanism-match** classification (cosine retrieval shortlists the top-20 nearest priors and futures; `gpt-5-mini` then makes the functional-equivalence call, catching paraphrases that cosine alone misses — calibrated on 78 cases to a 0.75 cosine threshold, 87% accuracy; the deterministic `cosine` backend uses cosine ≥ 0.75). The benchmark reports **mean and median set_score over the 24-snapshot test split** (a dev split of 16 snapshots is held out for iteration). *(The "+1.234 / +0.987" figures in the docs are illustrative, not published leaderboard results.)*

**What this means for winning strategy.** To maximize set_score an agent should: (a) **spread candidates across genuinely different mechanisms** (maximize pairwise cosine distance → diversity bonus, and avoid collapsing into one equivalence class); (b) **ground each candidate in real, retrievable literature/structure** so it plausibly matches a future-validated improvement (chasing the +impact×confidence reward — ideally frontier-idea-tier); (c) **avoid rediscovering already-tried priors**, especially ones that were failed/ruled-out/existence-killed (which carry the heaviest negative multipliers) — meaning the agent must actively model "what has already been tried" and steer *away* from it; (d) **always satisfy the structural contract** (title + `## Proposal` + ≥50 chars) to avoid the −1.0 invalid floor and to bank the validity term; and (e) when uncertain, prefer *mechanistically novel* proposals (worst case +0.3×confidence) over safe paraphrases of the current recipe (which risk rediscovery penalties).

---

## Recommendations — The Unified Operational Playbook

This is the step-by-step procedure an autonomous agent should run when asked to "generate novel ideas/hypotheses for problem X." It fuses first principles, analogy, blending, and the diverge/converge architecture, and is tuned to the NoveltyBench scoring rules.

**Phase 0 — Frame and reconnoiter (Executive/convergent).**
- Restate the problem and its goal/constraint in one sentence. Identify the *function* required (JTBD: what job must be done?), separate from current *form*.
- **Map what already exists / has been tried.** For autoresearch: actively retrieve priors and the current recipe; build an explicit "already-tried / failed / ruled-out" list. (On evo-hq this is the difference between rediscovery penalties and anticipation rewards.)

**Phase 1 — Decompose with first principles.**
- List every assumption and "best practice." Interrogate each: physics/logic, or mere convention? Reduce to irreducible truths (quantities, costs, real constraints). This frees the solution space.

**Phase 2 — Reframe.**
- Generate 3–5 alternative framings of the problem (invert it; change the level of abstraction; restate as a contradiction à la TRIZ; ask "what would make a better story?"). Each reframing opens a different region of idea space.

**Phase 3 — Diverge widely with multiple *distinct generators* (Generative phase — keep this loosely constrained, per Geneplore).** Deliberately run several *different* mechanisms so the candidates spread across equivalence classes rather than clustering:
- **(a) Cross-domain analogical transfer (highest-yield).** For each of several *distant* source domains (biology, physics, economics, game theory, evolution, thermodynamics, markets, ecology, immunology, linguistics), ask: "What mechanism in this domain solves a structurally analogous problem, and what would importing it look like here?" Map deep *relations*, not surface features (Gentner). This is the GAN/annealing/PageRank move.
- **(b) Remote association / random entry.** Inject random stimuli and force connections; deliberately sample low-probability associations (flatten your associative hierarchy — Mednick).
- **(c) Bisociation / conceptual blending.** Pick two unrelated frames, find their generic shared structure, project into a blend, and *run* the blend to read off emergent structure (Fauconnier & Turner).
- **(d) SCAMPER + TRIZ + morphological analysis.** Mechanically transform the current best solution (substitute/combine/reverse…), resolve its central contradiction via the 40 inventive principles, and combinatorially vary independent design dimensions (Zwicky box).
- **(e) Inversion & counterfactual.** Solve the opposite problem; ask "what if a core assumption were false?"
- **(f) Constraint injection.** Add an artificial severe constraint (half the budget, one moving part, must work offline) to force non-obvious solutions.
- **(g) Abductive hypothesis generation.** Surface the surprising anomalies/gaps; for each, ask "what mechanism, if true, would make this unsurprising?"
- Aim for *quantity and mechanistic variety*; suppress the editor (Kleon: don't edit ideas before you have them; this is the DMN-dominant phase).

**Phase 4 — Incubate / re-represent.**
- Between passes, change the representation or persona and regenerate (simulate incubation; surface associations the first frame suppressed). Run generators in parallel and pool all candidates with shared "what's been tried" state (mirroring the evo tree-search architecture).

**Phase 5 — Converge and select (Exploratory/Executive phase — ECN-dominant).**
- Cluster candidates into *mechanism* equivalence classes; **keep the best representative of each class and discard near-duplicates** (this directly protects distinct_k / diversity_bonus).
- Score each surviving candidate on **novelty × plausibility/quality** (utility): is it mechanistically distinct from priors? Is it grounded enough to plausibly be validated? Use Bayesian/calibrated-probability and steel-manning/red-team skills to evaluate; use pre-mortem to catch why each might fail.
- For autoresearch specifically: **drop anything that matches a failed/ruled-out prior** (heavy negative multiplier), prefer candidates that plausibly anticipate frontier improvements, and ensure each output meets the structural contract (title + `## Proposal` + ≥50 chars).

**Phase 6 — Output a diverse portfolio, not a single best answer.**
- Deliver N candidates deliberately spread across distinct generating mechanisms and source domains. This is the single most important behavioral shift versus default LLM mode-seeking: **optimize the *set*, not the single most-likely answer.**

**Staged adoption / benchmarks that change the strategy:**
- *If diversity scores (distinct_k / diversity_bonus) are low:* you are mode-collapsing — increase the number of distinct generators in Phase 3, raise sampling temperature/persona variation, and prune duplicates harder in Phase 5.
- *If novelty is high but quality/validation is low (many novel_unvalidated, few novel_validated):* invest more in literature grounding and structure-mapping fidelity so candidates land on real future-improvement threads.
- *If you incur rediscovery penalties:* your "already-tried" model is too weak — expand prior retrieval before generating, and explicitly steer away from failed/ruled-out families.
- *If you hit invalid (−1.0):* fix the output contract first; it dominates everything else.

---

## Caveats

- **Repo metadata is stale/contradictory.** The cc-thinking-skills GitHub "About" text and social badge say "18 mental models," but the README, the Skills-count badge, and the actual skill tables enumerate **39**. I cataloged all 39 from the README's tables and detailed sections; I did not open each individual `SKILL.md` file, so per-skill internal step lists are summarized from the README descriptions rather than quoted verbatim from each skill file.
- **The evo-hq scoring details come from the project's own README/documentation** (primary and authoritative for this benchmark) via a targeted secondary retrieval; the exact `scoring.py` weights were quoted from documentation rather than verified line-by-line in source. The illustrative numbers (mean +1.234, the cosine-0.73 example) are documentation examples, **not** published leaderboard results, and no public leaderboard scores were found.
- **The conceptual mapping between the evo-hq scaffold and the original NoveltyBench paper** (mechanism-distinct ≈ functional equivalence classes; diversity_bonus ≈ distinct_k; per-candidate score ≈ utility_k) is an analytical comparison; the evo-hq materials do not explicitly cite Zhang et al. (2504.05228).
- **Several innovation-origin stories are partly anecdotal.** Kekulé's snake-dream account is his own later retelling and is debated by historians; the Darwin-Malthus and Wright-brothers-birds influences are well-documented but were among several inputs, not sole causes. They remain valid *illustrations of structural transfer* regardless of historiographic detail.
- **Creativity neuroscience is correlational-leaning.** DMN–ECN coupling robustly *correlates* with creative performance (and the Chen et al. 2025 multi-center study links the *number* of DMN–ECN switches to creative ability), with some neurofeedback evidence pointing toward causation, but the field is young; treat network-level claims as strong working models, not settled mechanism.
- **Novelty is necessary but not sufficient.** Both benchmarks reward novelty *weighted by quality/validity*. An agent that maximizes diversity while ignoring grounding will score poorly on utility. The playbook's converge phase (and the quality-oriented cc-thinking skills) is therefore not optional.