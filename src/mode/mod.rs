use slint::ComponentHandle;
use std::ops::Deref;

pub mod launch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HideOnDrop<T: ComponentHandle>(pub T);

pub trait HideOnDropExt: ComponentHandle + Sized {
    fn hide_on_drop(self) -> HideOnDrop<Self>;
}

impl<T> HideOnDropExt for T
where
    T: ComponentHandle,
{
    fn hide_on_drop(self) -> HideOnDrop<Self> {
        HideOnDrop(self)
    }
}

impl<T> Drop for HideOnDrop<T>
where
    T: ComponentHandle,
{
    fn drop(&mut self) {
        let _ = self.0.hide();
    }
}

impl<T> Deref for HideOnDrop<T>
where
    T: ComponentHandle,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
