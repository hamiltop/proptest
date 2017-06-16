//-
// Copyright 2017 Jason Lingle
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::fmt;
use std::sync::Arc;

use rand;
use rand::distributions::IndependentSample;

use test_runner::*;

/// A strategy for producing arbitrary values of a given type.
pub trait Strategy {
    type Value : ValueTree;

    /// Generate a new value tree from the given runner.
    ///
    /// This may fail if there are constraints on the generated value and the
    /// generator is unable to produce anything that satisfies them. Any
    /// failure is wrapped in `TestError::Abort`.
    fn new_value
        (&self, runner: &mut TestRunner)
         -> Result<Self::Value, String>;

    /// Returns a strategy which produces values transformed by the function
    /// `fun`.
    ///
    /// There is no need (or possibility, for that matter) to define how the
    /// output is to be shrunken. Shrinking continues to take place in terms of
    /// the source value.
    fn prop_map<O : Clone + fmt::Debug,
                F : Fn (<Self::Value as ValueTree>::Value) -> O>
        (self, fun: F) -> Map<Self, F>
    where Self : Sized {
        Map { source: self, fun: Arc::new(fun) }
    }

    /// Returns a strategy which only produces values accepted by `fun`.
    ///
    /// This results in a very naïve form of rejection sampling and should only
    /// be used if (a) relatively few values will actually be rejected; (b) it
    /// isn't easy to express what you want by using another strategy and/or
    /// `map()`.
    ///
    /// There are a lot of downsides to this form of filtering. It slows
    /// testing down, since values must be generated but then discarded.
    /// Proptest only allows a limited number of rejects this way (across the
    /// entire `TestRunner`). Rejection can interfere with shrinking;
    /// particularly, complex filters may largely or entirely prevent shrinking
    /// from substantially altering the original value.
    ///
    /// Local rejection sampling is still preferable to rejecting the entire
    /// input to a test (via `TestCaseError::Reject`), however, and the default
    /// number of local rejections allowed is much higher than the number of
    /// whole-input rejections.
    ///
    /// `whence` is used to record where and why the rejection occurred.
    fn prop_filter<F : Fn (&<Self::Value as ValueTree>::Value) -> bool>
        (self, whence: String, fun: F) -> Filter<Self, F>
    where Self : Sized {
        Filter { source: self, whence: whence, fun: Arc::new(fun) }
    }

    /// Returns a strategy which picks uniformly from `self` and `other`.
    ///
    /// When shrinking, if a value from `other` was originally chosen but that
    /// value can be shrunken no further, it switches to a value from `self`
    /// and starts shrinking that.
    ///
    /// Be aware that chaining `prop_union` calls will result in a very
    /// right-skewed distribution. If this is not what you want, you can call
    /// the `.or()` method on the `Union` to add more values to the same union,
    /// or directly call `Union::new()`.
    ///
    /// Both `self` and `other` must be of the same type. To combine
    /// heterogeneous strategies, call the `boxed()` method on both `self` and
    /// `other` to erase the type differences before calling `prop_union()`.
    fn prop_union(self, other: Self) -> Union<Self>
    where Self : Sized {
        Union::new(vec![self, other])
    }

    /// Erases the type of this `Strategy` so it can be passed around as a
    /// simple trait object.
    fn boxed(self) -> BoxedStrategy<<Self::Value as ValueTree>::Value>
    where Self : Sized + 'static {
        Box::new(BoxedStrategyWrapper(self))
    }
}

impl<S : Strategy + ?Sized> Strategy for Box<S> {
    type Value = S::Value;

    fn new_value(&self, runner: &mut TestRunner)
                 -> Result<Self::Value, String>
    { (**self).new_value(runner) }
}

/// A generated value and its associated shrinker.
///
/// Conceptually, a `ValueTree` represents a spectrum between a "minimally
/// complex" value and a starting, randomly-chosen value. For values such as
/// numbers, this can be thought of as a simple binary search, and this is how
/// the `ValueTree` state machine is defined.
///
/// The `ValueTree` state machine notionally has three fields: low, current,
/// and high. Initially, low is the "minimally complex" value for the type, and
/// high and current are both the initially chosen value. It can be queried for
/// its current state. When shrinking, the controlling code tries simplifying
/// the value one step. If the test failure still happens with the simplified
/// value, further simplification occurs. Otherwise, the code steps back up
/// towards the prior complexity.
///
/// The main invariants here are that the "high" value always corresponds to a
/// failing test case, and that repeated calls to `complicate()` will return
/// `false` only once the "current" value has returned to what it was before
/// the last call to `simplify()`.
pub trait ValueTree {
    type Value : fmt::Debug;

    /// Returns the current value.
    fn current(&self) -> Self::Value;
    /// Attempts to simplify the current value. Notionally, this sets the
    /// "high" value to the current value, and the current value to a "halfway
    /// point" between high and low, rounding towards low.
    ///
    /// Returns whether any state changed as a result of this call.
    fn simplify(&mut self) -> bool;
    /// Attempts to partially undo the last simplification. Notionally, this
    /// sets the "low" value to one plus the current value, and the current
    /// value to a "halfway point" between high and the new low, rounding
    /// towards low.
    ///
    /// Returns whether any state changed as a result of this call.
    fn complicate(&mut self) -> bool;
}

impl<T : ValueTree + ?Sized> ValueTree for Box<T> {
    type Value = T::Value;
    fn current(&self) -> Self::Value { (**self).current() }
    fn simplify(&mut self) -> bool { (**self).simplify() }
    fn complicate(&mut self) -> bool { (**self).complicate() }
}

pub type BoxedStrategy<T> = Box<Strategy<Value = Box<ValueTree<Value = T>>>>;

struct BoxedStrategyWrapper<T>(T);
impl<T : Strategy> Strategy for BoxedStrategyWrapper<T>
where T::Value : 'static {
    type Value = Box<ValueTree<Value = <T::Value as ValueTree>::Value>>;

    fn new_value(&self, runner: &mut TestRunner)
        -> Result<Self::Value, String>
    {
        Ok(Box::new(self.0.new_value(runner)?))
    }
}

/// `Strategy` and `ValueTree` map adaptor.
///
/// See `Strategy::prop_map()`.
#[derive(Debug)]
pub struct Map<S, F> {
    source: S,
    fun: Arc<F>,
}

impl<S : Clone, F> Clone for Map<S, F> {
    fn clone(&self) -> Self {
        Map {
            source: self.source.clone(),
            fun: self.fun.clone(),
        }
    }
}

impl<S : Strategy, O : Clone + fmt::Debug,
     F : Fn (<S::Value as ValueTree>::Value) -> O>
Strategy for Map<S, F> {
    type Value = Map<S::Value, F>;

    fn new_value(&self, runner: &mut TestRunner)
                 -> Result<Self::Value, String> {
        self.source.new_value(runner).map(
            |v| Map { source: v, fun: self.fun.clone() })
    }
}

impl<S : ValueTree, O : Clone + fmt::Debug, F : Fn (S::Value) -> O>
ValueTree for Map<S, F> {
    type Value = O;

    fn current(&self) -> O {
        (self.fun)(self.source.current())
    }

    fn simplify(&mut self) -> bool {
        self.source.simplify()
    }

    fn complicate(&mut self) -> bool {
        self.source.complicate()
    }
}

/// `Strategy` and `ValueTree` filter adaptor.
///
/// See `Strategy::prop_filter()`.
#[derive(Debug)]
pub struct Filter<S, F> {
    source: S,
    whence: String,
    fun: Arc<F>,
}

impl<S : Clone, F> Clone for Filter<S, F> {
    fn clone(&self) -> Self {
        Filter {
            source: self.source.clone(),
            whence: self.whence.clone(),
            fun: self.fun.clone(),
        }
    }
}

impl<S : Strategy,
     F : Fn (&<S::Value as ValueTree>::Value) -> bool>
Strategy for Filter<S, F> {
    type Value = Filter<S::Value, F>;

    fn new_value(&self, runner: &mut TestRunner)
                 -> Result<Self::Value, String> {
        loop {
            let val = self.source.new_value(runner)?;
            if !(self.fun)(&val.current()) {
                runner.reject_local(self.whence.clone())?;
            } else {
                return Ok(Filter {
                    source: val,
                    whence: self.whence.clone(),
                    fun: self.fun.clone(),
                })
            }
        }
    }
}

impl<S : ValueTree, F : Fn (&S::Value) -> bool>
Filter<S, F> {
    fn ensure_acceptable(&mut self) {
        while !(self.fun)(&self.source.current()) {
            if !self.source.complicate() {
                panic!("Unable to complicate filtered strategy \
                        back into acceptable value");
            }
        }
    }
}

impl<S : ValueTree, F : Fn (&S::Value) -> bool>
ValueTree for Filter<S, F> {
    type Value = S::Value;

    fn current(&self) -> S::Value {
        self.source.current()
    }

    fn simplify(&mut self) -> bool {
        if self.source.simplify() {
            self.ensure_acceptable();
            true
        } else {
            false
        }
    }

    fn complicate(&mut self) -> bool {
        if self.source.complicate() {
            self.ensure_acceptable();
            true
        } else {
            false
        }
    }
}

/// A `Strategy` which picks from one of several delegate `Stragegy`s.
///
/// See `Strategy::prop_union()`.
#[derive(Clone, Debug)]
pub struct Union<T : Strategy> {
    options: Vec<T>,
}

impl<T : Strategy> Union<T> {
    /// Create a strategy which selects uniformly from the given delegate
    /// strategies.
    ///
    /// When shrinking, after maximal simplification of the chosen element, the
    /// strategy will move to earlier options and continue simplification with
    /// those.
    ///
    /// ## Panics
    ///
    /// Panics if `options` is empty.
    pub fn new(options: Vec<T>) -> Self {
        assert!(options.len() > 0);

        Union { options: options }
    }

    /// Add `other` as an additional alternate strategy.
    pub fn or(mut self, other: T) -> Self {
        self.options.push(other);
        self
    }
}

impl<T : Strategy> Strategy for Union<T> {
    type Value = UnionValueTree<T::Value>;

    fn new_value(&self, runner: &mut TestRunner)
                 -> Result<Self::Value, String> {
        let pick = rand::distributions::Range::new(0, self.options.len())
            .ind_sample(runner.rng());

        let mut options = Vec::with_capacity(pick);
        for option in &self.options {
            options.push(option.new_value(runner)?);
        }

        Ok(UnionValueTree {
            options: options,
            pick: pick,
            min_pick: 0,
            prev_pick: None,
        })
    }
}

/// `ValueTree` corresponding to `Union`.
#[derive(Clone, Debug)]
pub struct UnionValueTree<T : ValueTree> {
    options: Vec<T>,
    pick: usize,
    min_pick: usize,
    prev_pick: Option<usize>,
}

impl<T : ValueTree> ValueTree for UnionValueTree<T> {
    type Value = T::Value;

    fn current(&self) -> T::Value {
        self.options[self.pick].current()
    }

    fn simplify(&mut self) -> bool {
        if self.options[self.pick].simplify() {
            self.prev_pick = None;
            true
        } else if self.pick > self.min_pick {
            self.prev_pick = Some(self.pick);
            self.pick -= 1;
            true
        } else {
            false
        }
    }

    fn complicate(&mut self) -> bool {
        if let Some(pick) = self.prev_pick {
            self.pick = pick;
            self.min_pick = pick;
            self.prev_pick = None;
            true
        } else {
            self.options[self.pick].complicate()
        }
    }
}

/// A `Strategy` which always produces the same value and never simplifies.
#[derive(Clone, Copy, Debug)]
pub struct Singleton<T : Clone + fmt::Debug>(
    /// The value produced by this strategy.
    pub T);

impl<T : Clone + fmt::Debug> Strategy for Singleton<T> {
    type Value = Self;

    fn new_value(&self, _: &mut TestRunner) -> Result<Self::Value, String> {
        Ok(self.clone())
    }
}

impl<T : Clone + fmt::Debug> ValueTree for Singleton<T> {
    type Value = T;

    fn current(&self) -> T {
        self.0.clone()
    }

    fn simplify(&mut self) -> bool { false }
    fn complicate(&mut self) -> bool { false }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_map() {
        TestRunner::new(Config::default())
            .run(&(0..10).prop_map(|v| v * 2), |&v| {
                assert!(0 == v % 2);
                Ok(())
            }).unwrap();
    }

    #[test]
    fn test_filter() {
        let input = (0..256).prop_filter("%3".to_owned(), |&v| 0 == v % 3);

        for _ in 0..256 {
            let mut runner = TestRunner::new(Config::default());
            let mut case = input.new_value(&mut runner).unwrap();

            assert!(0 == case.current() % 3);

            while case.simplify() {
                assert!(0 == case.current() % 3);
            }
            assert!(0 == case.current() % 3);
        }
    }

    #[test]
    fn test_union() {
        let input = (10u32..20u32).prop_union(30u32..40u32);
        // Expect that 25% of cases pass (left input happens to be < 15, and
        // left is chosen as initial value). Of the 75% that fail, 50% should
        // converge to 15 and 50% to 30 (the latter because the left is beneath
        // the passing threshold).
        let mut passed = 0;
        let mut converged_low = 0;
        let mut converged_high = 0;
        for _ in 0..256 {
            let mut runner = TestRunner::new(Config::default());
            let case = input.new_value(&mut runner).unwrap();
            let result = runner.run_one(case, |&v| if v < 15 {
                Ok(())
            } else {
                Err(TestCaseError::Fail("fail".to_owned()))
            });

            match result {
                Ok(true) => passed += 1,
                Err(TestError::Fail(_, 15)) => converged_low += 1,
                Err(TestError::Fail(_, 30)) => converged_high += 1,
                e => panic!("Unexpected result: {:?}", e),
            }
        }

        assert!(passed >= 32 && passed <= 96,
                "Bad passed count: {}", passed);
        assert!(converged_low >= 32 && converged_low <= 160,
                "Bad converged_low count: {}", converged_low);
        assert!(converged_high >= 32 && converged_high <= 160,
                "Bad converged_high count: {}", converged_high);
    }
}
