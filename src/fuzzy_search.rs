/// Abstraction over the nucleo fuzzy matching engine.
///
/// 'a: how long references to the matched data live
/// C: how many columns to match against
/// D: type of matched entries
pub struct FuzzySearch<const C: usize, D: Sync + Send + 'static> {
    // inner fuzzy matcher
    nucleo: nucleo::Nucleo<D>,
    injector: nucleo::Injector<D>,
    // notification semaphore for when nucleo results are available;
    // notified any time a user may read matches and get a new result from it
    notify: crate::notify::Notify,
    query: [String; C],
}

pub trait Row<const C: usize> {
    type Output;

    fn columns(&self) -> [Self::Output; C];
}

impl<const C: usize, D: Sync + Send + 'static> FuzzySearch<C, D> {
    /// Start a new search.
    pub fn search<const COL: usize>(&mut self, query: impl Into<String>) {
        let query = query.into();
        // is the old query a prefix of the new one?
        // if true, this enables optimizations in the matcher.
        let append = query.starts_with(self.query[COL].as_str());
        self.nucleo.pattern.reparse(
            COL,
            query.as_str(),
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Never,
            append,
        );
        // update the internal query
        self.query[COL] = query;
        // update the matcher
        let status = self.tick();
        if !status.running && status.changed {
            // somehow, the worker finished immediately,
            // so immediately notify of the results.
            self.notify.notify();
        }
    }

    /// Collects the matches from the matching engine
    pub fn get_matches(&self) -> Vec<&D> {
        let snapshot = self.nucleo.snapshot();
        let matched = snapshot
            .matched_items(..)
            // .filter(|m| m.idx != u32::MAX) // I don't know why this would occasionally happen, but it would panic.
            // .filter_map(|m| snapshot.get_item(m.idx))
            .map(|item| item.data)
            .collect();

        matched
    }

    pub fn tick(&mut self) -> nucleo::Status {
        self.nucleo.tick(0)
    }

    pub fn notify(&self) -> crate::notify::Notify {
        self.notify.clone()
    }

    /// Access the inner nucleo [nucleo::Injector]
    #[inline]
    #[expect(unused)]
    pub fn injector(&self) -> &nucleo::Injector<D> {
        &self.injector
    }
}

impl<const C: usize, D: Sync + Send + 'static> FuzzySearch<C, D>
where
    D: Row<C>,
    D::Output: Into<nucleo::Utf32String>,
{
    /// Create a new [FuzzySearch] with the provided nucleo configuration
    pub fn create_with_config(config: nucleo::Config) -> Self {
        let notify = crate::notify::Notify::new();
        let nucleo = {
            let notify = notify.clone();
            nucleo::Nucleo::new(
                config,
                std::sync::Arc::new(move || notify.notify()),
                None,
                C as u32,
            )
        };
        let injector = nucleo.injector();

        Self {
            nucleo,
            injector,
            notify,
            query: [const { String::new() }; _],
        }
    }

    fn push_into(injector: &nucleo::Injector<D>, entry: D) -> u32 {
        injector.push(entry, |entry: &D, col: &mut [nucleo::Utf32String]| {
            // for this entry, get the column values from its Row implementation
            let strings = entry.columns();
            // turn them into nucleo::Utf32String
            // (Into impl comes from trait bound on D)
            // --
            // technically we already have the heap-allocations of Utf32String in `col` at this point,
            // so it coooouuulld be more efficient to fill & grow those instead,
            // but who cares?
            let mut strings = strings.map(|output| output.into());
            col.swap_with_slice(&mut strings);
        })
    }

    /// Returns a function that may be called to push items into the fuzzy matcher.
    /// This exists as a simple handle that can be given to an async task, instead of
    /// requiring shared ownership of the [FuzzySearch]
    #[expect(unused)]
    pub fn pusher(&self) -> impl Fn(D) + Send + Sync {
        let injector = self.injector.clone();
        move |entry: D| {
            Self::push_into(&injector, entry);
        }
    }

    /// Add an entry to the matcher.
    pub fn push(&self, entry: D) {
        Self::push_into(&self.injector, entry);
    }

    /// Add a bunch of entries to the matcher.
    #[expect(unused)]
    pub fn push_all(&self, iter: impl IntoIterator<Item = D>) {
        iter.into_iter().for_each(|i| self.push(i))
    }
}
