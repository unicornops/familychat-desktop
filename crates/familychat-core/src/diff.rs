//! [`ListDiff`]: one change to a list a frontend keeps (the room list, a
//! timeline), the session's copy of eyeball-im's `VectorDiff` without the
//! SDK type (PLAN.md, section 5.2).
//!
//! The session sends diffs in batches, in the order the SDK produced them;
//! a frontend applies each batch to its own `Vec` with [`apply_all`] and
//! redraws once. Coalescing bursts (several batches before one frame) is the
//! frontend's job: it drains what is waiting with `try_recv` and notifies
//! once.

/// One change to a list, as eyeball-im's `VectorDiff` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListDiff<T> {
    /// Append these values at the end.
    Append {
        /// The values, in order.
        values: Vec<T>,
    },
    /// Remove every value.
    Clear,
    /// Insert a value at the front.
    PushFront {
        /// The value.
        value: T,
    },
    /// Append a value at the end.
    PushBack {
        /// The value.
        value: T,
    },
    /// Remove the first value.
    PopFront,
    /// Remove the last value.
    PopBack,
    /// Insert a value at `index`, moving the later ones along.
    Insert {
        /// Where, at most the list's length.
        index: usize,
        /// The value.
        value: T,
    },
    /// Replace the value at `index`.
    Set {
        /// Which value.
        index: usize,
        /// The new value.
        value: T,
    },
    /// Remove the value at `index`.
    Remove {
        /// Which value.
        index: usize,
    },
    /// Keep only the first `length` values.
    Truncate {
        /// How many to keep.
        length: usize,
    },
    /// Replace the whole list.
    Reset {
        /// The new list.
        values: Vec<T>,
    },
}

/// A diff named a position the list does not have: the frontend's copy has
/// gone out of step and should ask for the list again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffOutOfRange {
    /// The position the diff named.
    pub index: usize,
    /// The list's length when it was applied.
    pub len: usize,
}

impl std::fmt::Display for DiffOutOfRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a list update named position {} of a list of {}",
            self.index, self.len
        )
    }
}

impl std::error::Error for DiffOutOfRange {}

impl<T> ListDiff<T> {
    /// The same change with every value converted by `convert`.
    pub fn map<U>(self, mut convert: impl FnMut(T) -> U) -> ListDiff<U> {
        match self {
            Self::Append { values } => ListDiff::Append {
                values: values.into_iter().map(&mut convert).collect(),
            },
            Self::Clear => ListDiff::Clear,
            Self::PushFront { value } => ListDiff::PushFront {
                value: convert(value),
            },
            Self::PushBack { value } => ListDiff::PushBack {
                value: convert(value),
            },
            Self::PopFront => ListDiff::PopFront,
            Self::PopBack => ListDiff::PopBack,
            Self::Insert { index, value } => ListDiff::Insert {
                index,
                value: convert(value),
            },
            Self::Set { index, value } => ListDiff::Set {
                index,
                value: convert(value),
            },
            Self::Remove { index } => ListDiff::Remove { index },
            Self::Truncate { length } => ListDiff::Truncate { length },
            Self::Reset { values } => ListDiff::Reset {
                values: values.into_iter().map(convert).collect(),
            },
        }
    }

    /// Applies the change to `list`. A change at a position the list does
    /// not have leaves it untouched and is an error.
    pub fn apply(self, list: &mut Vec<T>) -> Result<(), DiffOutOfRange> {
        let len = list.len();
        let out_of_range = |index| DiffOutOfRange { index, len };
        match self {
            Self::Append { values } => list.extend(values),
            Self::Clear => list.clear(),
            Self::PushFront { value } => list.insert(0, value),
            Self::PushBack { value } => list.push(value),
            Self::PopFront => {
                if list.is_empty() {
                    return Err(out_of_range(0));
                }
                list.remove(0);
            }
            Self::PopBack => {
                if list.pop().is_none() {
                    return Err(out_of_range(0));
                }
            }
            Self::Insert { index, value } => {
                if index > len {
                    return Err(out_of_range(index));
                }
                list.insert(index, value);
            }
            Self::Set { index, value } => {
                let slot = list.get_mut(index).ok_or(out_of_range(index))?;
                *slot = value;
            }
            Self::Remove { index } => {
                if index >= len {
                    return Err(out_of_range(index));
                }
                list.remove(index);
            }
            Self::Truncate { length } => list.truncate(length),
            Self::Reset { values } => *list = values,
        }
        Ok(())
    }
}

/// Applies a batch of changes in order, stopping at the first that does not
/// fit.
pub fn apply_all<T>(
    list: &mut Vec<T>,
    diffs: impl IntoIterator<Item = ListDiff<T>>,
) -> Result<(), DiffOutOfRange> {
    for diff in diffs {
        diff.apply(list)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_every_kind_of_change() {
        let mut list = vec![1, 2, 3];
        apply_all(
            &mut list,
            [
                ListDiff::PushFront { value: 0 },
                ListDiff::PushBack { value: 4 },
                ListDiff::Insert { index: 2, value: 9 },
                ListDiff::Set {
                    index: 0,
                    value: 10,
                },
                ListDiff::Remove { index: 1 },
                ListDiff::Append { values: vec![5, 6] },
                ListDiff::PopFront,
                ListDiff::PopBack,
                ListDiff::Truncate { length: 4 },
            ],
        )
        .expect("every change fits");
        assert_eq!(list, [9, 2, 3, 4]);
        apply_all(&mut list, [ListDiff::Clear]).expect("clears");
        assert!(list.is_empty());
        apply_all(&mut list, [ListDiff::Reset { values: vec![7] }]).expect("resets");
        assert_eq!(list, [7]);
    }

    #[test]
    fn a_change_out_of_range_is_an_error_and_changes_nothing() {
        let mut list = vec![1];
        assert_eq!(
            ListDiff::Set { index: 1, value: 2 }.apply(&mut list),
            Err(DiffOutOfRange { index: 1, len: 1 })
        );
        assert!(
            ListDiff::Insert { index: 2, value: 2 }
                .apply(&mut list)
                .is_err()
        );
        assert!(ListDiff::Remove { index: 1 }.apply(&mut list).is_err());
        assert_eq!(list, [1]);
        let mut empty: Vec<i32> = Vec::new();
        assert!(ListDiff::PopFront.apply(&mut empty).is_err());
        assert!(ListDiff::PopBack.apply(&mut empty).is_err());
    }

    #[test]
    fn maps_values_and_keeps_positions() {
        assert_eq!(
            ListDiff::Insert { index: 3, value: 2 }.map(|value| value * 10),
            ListDiff::Insert {
                index: 3,
                value: 20
            }
        );
        assert_eq!(
            ListDiff::Reset { values: vec![1, 2] }.map(|value| value.to_string()),
            ListDiff::Reset {
                values: vec!["1".to_owned(), "2".to_owned()]
            }
        );
    }
}
