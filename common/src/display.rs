use std::fmt;

pub struct OptDisplay<T> {
	text: &'static str,
	value: Option<T>,
}

impl<T> OptDisplay<T> {
	pub fn new(text: &'static str, value: Option<T>) -> Self {
		Self { text, value }
	}
}

impl<T> fmt::Display for OptDisplay<T>
where
	T: fmt::Display,
{
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match &self.value {
			Some(v) => fmt::Display::fmt(&self.text.replace("{}", &format!("{}", v)), f),
			None => Ok(()),
		}
	}
}

pub trait MaybeDisplay<T>: Sized {
	fn display(self, text: &'static str) -> OptDisplay<T>;

	fn format(self, text: &'static str) -> String
	where
		T: fmt::Display,
	{
		format!("{}", self.display(text))
	}
}

impl<'a, T> MaybeDisplay<&'a T> for Option<&'a T>
where
	T: fmt::Display,
{
	fn display(self, text: &'static str) -> OptDisplay<&'a T> {
		OptDisplay::new(text, self)
	}
}
