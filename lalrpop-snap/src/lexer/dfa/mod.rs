//! Constructs a DFA which picks the longest matching regular
//! expression from the input.

use kernel_set::{Kernel, KernelSet};
use std::fmt::{Debug, Display, Formatter, Error};
use std::rc::Rc;
use lexer::re;
use lexer::nfa::{self, NFA, NFAStateIndex};
use util::Set;

#[cfg(test)]
mod test;

#[cfg(test)]
mod interpret;

#[derive(Debug)]
pub struct DFA {
    states: Vec<State>
}

#[derive(Copy, Clone, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct Precedence(usize);

pub fn build_dfa(regexs: &[re::Regex],
                 precedences: &[Precedence])
                 -> Result<DFA, Ambiguity> {
    assert_eq!(regexs.len(), precedences.len());
    let nfas: Vec<_> = regexs.iter().map(|r| NFA::from_re(r)).collect();
    let builder = DFABuilder { nfas: &nfas, precedences: precedences.to_vec() };
    let dfa = builder.build();
    dfa
}

struct DFABuilder<'nfa> {
    nfas: &'nfa [NFA],
    precedences: Vec<Precedence>,
}

#[derive(Debug)]
struct State {
    item_set: DFAItemSet,
    kind: Kind,
    test_edges: Vec<(re::Test, DFAStateIndex)>,
    other_edge: DFAStateIndex,
}

#[derive(Debug)]
enum Kind {
    Accepts(NFAIndex),
    Reject,
    Neither,
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct NFAIndex(usize);

#[derive(Copy, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct DFAStateIndex(usize);

type DFAKernelSet = KernelSet<DFAItemSet>;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct DFAItemSet {
    items: Rc<Vec<Item>>
}

#[derive(Copy, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
struct Item {
    // which regular expression?
    nfa_index: NFAIndex,

    // what state within the NFA are we at?
    nfa_state: NFAStateIndex,
}

const START: DFAStateIndex = DFAStateIndex(0);

/// Either of the two regexs listed could match, and they have equal
/// priority.
#[derive(Debug)]
pub struct Ambiguity {
    match0: NFAIndex, match1: NFAIndex
}

impl<'nfa> DFABuilder<'nfa> {
    fn build(&self) -> Result<DFA, Ambiguity> {
        let mut kernel_set = KernelSet::new();
        let mut states = vec![];

        let start_state_index = self.start_state(&mut kernel_set);
        assert_eq!(start_state_index, START);

        while let Some(item_set) = kernel_set.next() {
            // collect all the specific tests we expect from any of
            // the items in this state
            let tests: Set<re::Test> =
                item_set.items
                        .iter()
                        .flat_map(|&item| {
                            self.nfa(item)
                                .edges::<re::Test>(item.nfa_state)
                                .map(|edge| edge.label)
                        })
                        .collect();

            // if any NFA is in an accepting state, that makes this
            // DFA state an accepting state
            let mut all_accepts: Vec<(Precedence, NFAIndex)> =
                item_set.items
                        .iter()
                        .cloned()
                        .filter(|&item| self.nfa(item).is_accepting_state(item.nfa_state))
                        .map(|item| (self.precedences[item.nfa_index.0], item.nfa_index))
                        .collect();

            // if all NFAs are in a rejecting state, that makes this
            // DFA a rejecting state
            let all_rejects: bool =
                item_set.items
                        .iter()
                        .all(|&item| self.nfa(item).is_rejecting_state(item.nfa_state));

            let kind = if all_rejects {
                Kind::Reject
            } else if all_accepts.len() == 0 {
                Kind::Neither
            } else if all_accepts.len() == 1 {
                // accepts just one NFA, easy case
                Kind::Accepts(all_accepts[0].1)
            } else {
                all_accepts.sort(); // sort regex with higher precedence, well, higher
                let (best_priority, best_nfa) = all_accepts[all_accepts.len() - 1];
                let (next_priority, next_nfa) = all_accepts[all_accepts.len() - 2];
                if best_priority == next_priority {
                    return Err(Ambiguity { match0: best_nfa, match1: next_nfa });
                }
                Kind::Accepts(best_nfa)
            };

            // for each specific test, find what happens if we see a
            // character matching that test
            let test_edges: Vec<(re::Test, DFAStateIndex)> =
                tests.iter()
                     .map(|&test| {
                         let items: Vec<_> =
                             item_set.items.iter()
                                           .filter_map(|&item| self.accept_test(item, test))
                                           .collect();

                         // at least one of those items should accept this test
                         assert!(!items.is_empty());

                         (test, kernel_set.add_state(self.transitive_closure(items)))
                     })
                     .collect();

            // Consider what there is some cahracter that doesn't meet
            // any of the tests. In this case, we can just ignore all
            // the test edges for each of the items and just union all
            // the "other" edges -- because if it were one of those
            // test edges, then that transition is represented above.
            let other_transitions: Vec<_> =
                item_set.items.iter()
                              .filter_map(|&item| self.accept_other(item))
                              .collect();

            // we never know the full set
            assert!(!other_transitions.is_empty());

            let other_edge = kernel_set.add_state(self.transitive_closure(other_transitions));

            let state = State {
                item_set: item_set,
                kind: kind,
                test_edges: test_edges,
                other_edge: other_edge,
            };

            states.push(state);
        }

        Ok(DFA { states: states })
    }

    fn start_state(&self, kernel_set: &mut DFAKernelSet) -> DFAStateIndex {
        // starting state is at the beginning of all regular expressions
        let items: Vec<_> =
            (0..self.nfas.len())
            .map(|i| Item { nfa_index: NFAIndex(i),
                            nfa_state: nfa::START })
            .collect();
        let item_set = self.transitive_closure(items);
        kernel_set.add_state(item_set)
    }

    fn accept_test(&self, item: Item, test: re::Test) -> Option<Item> {
        let nfa = self.nfa(item);

        let matching_test =
            nfa.edges::<re::Test>(item.nfa_state)
               .filter(|edge| edge.label.meets(test))
               .map(|edge| item.to(edge.to));

        let matching_other =
            nfa.edges::<nfa::Other>(item.nfa_state)
               .map(|edge| item.to(edge.to));

        matching_test.chain(matching_other).next()
    }

    fn accept_other(&self, item: Item) -> Option<Item> {
        let nfa = self.nfa(item);
        nfa.edges::<nfa::Other>(item.nfa_state)
            .map(|edge| item.to(edge.to))
            .next()
    }

    fn transitive_closure(&self, mut items: Vec<Item>) -> DFAItemSet {
        let mut observed: Set<Item> = items.iter().cloned().collect();

        let mut counter = 0;
        while counter < items.len() {
            let item = items[counter];
            let derived_states =
                self.nfa(item)
                    .edges::<nfa::Noop>(item.nfa_state)
                    .map(|edge| item.to(edge.to))
                    .filter(|&item| observed.insert(item));
            items.extend(derived_states);
            counter += 1;
        }

        items.sort();
        items.dedup();

        DFAItemSet { items: Rc::new(items) }
    }

    fn nfa(&self, item: Item) -> &NFA {
        &self.nfas[item.nfa_index.0]
    }
}

impl Kernel for DFAItemSet {
    type Index = DFAStateIndex;

    fn index(c: usize) -> DFAStateIndex {
        DFAStateIndex(c)
    }
}

impl DFA {
    fn state(&self, index: DFAStateIndex) -> &State {
        &self.states[index.0]
    }
}

impl Item {
    fn to(&self, s: NFAStateIndex) -> Item {
        Item { nfa_index: self.nfa_index, nfa_state: s }
    }
}

impl Debug for DFAStateIndex {
    fn fmt(&self, fmt: &mut Formatter) -> Result<(), Error> {
        write!(fmt, "DFA{}", self.0)
    }
}

impl Display for DFAStateIndex {
    fn fmt(&self, fmt: &mut Formatter) -> Result<(), Error> {
        Debug::fmt(self, fmt)
    }
}

impl NFAIndex {
    fn index(&self) -> usize {
        self.0
    }
}

impl Debug for Item {
    fn fmt(&self, fmt: &mut Formatter) -> Result<(), Error> {
        write!(fmt, "({:?}:{:?})", self.nfa_index, self.nfa_state)
    }
}
