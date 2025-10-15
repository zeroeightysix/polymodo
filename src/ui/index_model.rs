use indexmap::IndexMap;
use slint::{Model, ModelNotify, ModelTracker};
use std::any::Any;
use std::cell::RefCell;
use std::hash::Hash;

pub struct IndexModel<K, V> {
    map: RefCell<IndexMap<K, V>>,
    notify: ModelNotify,
}

impl<K, V> Default for IndexModel<K, V> {
    fn default() -> Self {
        Self {
            map: Default::default(),
            notify: Default::default(),
        }
    }
}

#[allow(unused)]
impl<K, V> IndexModel<K, V> {
    pub fn mutate_row<R>(&self, row: usize, fun: impl FnOnce(&K, &mut V) -> R) -> Option<R> {
        let mut map = self.map.borrow_mut();
        let (k, v) = map.get_index_mut(row)?;
        let r = fun(k, v);
        drop(map);

        self.notify.row_changed(row);

        Some(r)
    }

    pub fn mutate_all(&self, mut fun: impl FnMut(usize, &K, &mut V)) {
        let mut map = self.map.borrow_mut();
        for (idx, (k, v)) in map.iter_mut().enumerate() {
            fun(idx, k, v);
        }
        drop(map);

        self.notify.reset();
    }

    pub fn mutate_by_key<Q, R>(
        &self,
        key: &Q,
        fun: impl FnOnce(usize, &K, &mut V) -> R,
    ) -> Option<R>
    where
        Q: ?Sized + Hash + indexmap::Equivalent<K>,
    {
        let mut map = self.map.borrow_mut();
        let (row, k, v) = map.get_full_mut(key)?;
        let r = fun(row, k, v);
        drop(map);

        self.notify.row_changed(row);

        Some(r)
    }
}

impl<K: Clone, V> IndexModel<K, V> {
    fn get_row_key(&self, row: usize) -> Option<K> {
        self.map.borrow().get_index(row).map(|(k, _v)| k).cloned()
    }
}

impl<K: Clone, V: Clone> IndexModel<K, V> {
    #[expect(unused)]
    fn get_row_key_value(&self, row: usize) -> Option<(K, V)> {
        self.map
            .borrow()
            .get_index(row)
            .map(|(k, v)| (k.clone(), v.clone()))
    }
}

impl<K: Hash + Eq, V> IndexModel<K, V> {
    pub fn insert(&self, key: K, value: V) {
        self.map.borrow_mut().insert(key, value);
    }

    #[expect(unused)]
    pub fn get_row_of_key<Q>(&self, key: &Q) -> Option<usize>
    where
        Q: ?Sized + Hash + indexmap::Equivalent<K>,
    {
        let idx = self.map.borrow().get_index_of(key)?;

        Some(idx)
    }
}

impl<K: Hash + Eq, V: Clone> IndexModel<K, V> {
    pub fn get_value_of_key<Q>(&self, key: &Q) -> Option<V>
    where
        Q: ?Sized + Hash + indexmap::Equivalent<K>,
    {
        let idx = self.map.borrow().get(key).cloned()?;

        Some(idx)
    }
}

impl<K: 'static, V: Clone + 'static> Model for IndexModel<K, V> {
    type Data = V;

    fn row_count(&self) -> usize {
        self.map.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<Self::Data> {
        self.map.borrow().get_index(row).map(|(_k, v)| v).cloned()
    }

    fn set_row_data(&self, row: usize, data: Self::Data) {
        self.map.borrow_mut()[row] = data;
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod test {}
