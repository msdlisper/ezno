use std::borrow::Cow;

use crate::tsx_keywords::New;
use crate::{
	errors::parse_lexing_error, expressions::TemplateLiteralPart,
	extensions::decorators::Decorated, CursorId, Decorator, Keyword, ParseResult, TypeId,
	VariableField, VariableFieldInTypeReference, WithComment,
};
use crate::{parse_bracketed, to_string_bracketed};
use derive_partial_eq_extras::PartialEqExtras;
use iterator_endiate::EndiateIteratorExt;

use super::{
	interface::{parse_interface_members, InterfaceMember},
	type_declarations::GenericTypeConstraint,
};

use crate::{
	tokens::token_as_identifier, ASTNode, NumberStructure, ParseError, ParseSettings, Span,
	TSXKeyword, TSXToken, Token, TokenReader,
};

/// A reference to a type
///
/// TODO need to figure out what [TypeId] is used for here and where it might be useful for the checker
#[derive(Debug, Clone, PartialEqExtras, Eq)]
#[partial_eq_ignore_types(Span, TypeId)]
#[cfg_attr(feature = "self-rust-tokenize", derive(self_rust_tokenize::SelfRustTokenize))]
pub enum TypeReference {
	/// A name e.g. `IPost`
	Name(String, Span),
	/// A name e.g. `Intl.IPost`. TODO can there be more than 2 members
	NamespacedName(String, String, Span),
	/// A name with generics e.g. `Array<number>`
	NameWithGenericArguments(String, Vec<TypeReference>, Span),
	/// Union e.g. `number | string`
	Union(Vec<TypeReference>),
	/// Intersection e.g. `c & d`
	Intersection(Vec<TypeReference>),
	/// String literal e.g. `"foo"`
	StringLiteral(String, Span),
	/// Number literal e.g. `45`
	NumberLiteral(NumberStructure, Span),
	/// Boolean literal e.g. `true`
	BooleanLiteral(bool, Span),
	/// Array literal e.g. `string[]`. This is syntactic sugar for `Array` with type arguments. **This is not the same
	/// as a [TypeReference::TupleLiteral]**
	ArrayLiteral(Box<TypeReference>, Span),
	/// Function literal e.g. `(x: string) => string`
	FunctionLiteral {
		type_parameters: Option<Vec<GenericTypeConstraint>>,
		parameters: TypeReferenceFunctionParameters,
		return_type: Box<TypeReference>,
		/// TODO...
		type_id: TypeId,
	},
	/// Construction literal e.g. `new (x: string) => string`
	ConstructorLiteral {
		new_keyword: Keyword<New>,
		type_parameters: Option<Vec<GenericTypeConstraint>>,
		parameters: TypeReferenceFunctionParameters,
		return_type: Box<TypeReference>,
	},
	/// Object literal e.g. `{ y: string }`
	/// Here [TypeId] refers to the type it declares
	ObjectLiteral(Vec<Decorated<InterfaceMember>>, TypeId, Span),
	/// Tuple literal e.g. `[number, x: string]`
	TupleLiteral(Vec<TupleElement>, TypeId, Span),
	///
	TemplateLiteral(Vec<TemplateLiteralPart<TypeReference>>, Span),
	/// Declares type as not assignable (still has interior mutability) e.g. `readonly number`
	Readonly(Box<TypeReference>, Span),
	/// Declares type as being union type of all property types e.g. `T[K]`
	Index(Box<TypeReference>, Box<TypeReference>, Span),
	/// KeyOf
	KeyOf(Box<TypeReference>, Span),
	/// For operation precedence reasons
	ParenthesizedReference(Box<TypeReference>, Span),
	Conditional {
		condition: TypeCondition,
		resolve_true: TypeConditionResult,
		resolve_false: TypeConditionResult,
		position: Span,
	},
	Decorated(Decorator, Box<Self>, Span),
	#[self_tokenize_field(0)]
	Cursor(CursorId<TypeReference>, Span),
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "self-rust-tokenize", derive(self_rust_tokenize::SelfRustTokenize))]
pub enum TupleElement {
	NonSpread { name: Option<String>, ty: TypeReference },
	Spread { name: Option<String>, ty: TypeReference },
}

/// Condition in a [TypeReference::Conditional]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "self-rust-tokenize", derive(self_rust_tokenize::SelfRustTokenize))]
pub enum TypeCondition {
	Extends { r#type: Box<TypeReference>, extends: Box<TypeReference>, position: Span },
	Is { r#type: Box<TypeReference>, is: Box<TypeReference>, position: Span },
}

impl TypeCondition {
	pub(crate) fn to_string_from_buffer<T: source_map::ToString>(
		&self,
		buf: &mut T,
		settings: &crate::ToStringSettingsAndData,
		depth: u8,
	) {
		match self {
			TypeCondition::Extends { r#type, extends, .. } => {
				r#type.to_string_from_buffer(buf, settings, depth);
				buf.push_str(" extends ");
				extends.to_string_from_buffer(buf, settings, depth);
			}
			TypeCondition::Is { r#type, is, .. } => {
				r#type.to_string_from_buffer(buf, settings, depth);
				buf.push_str(" is ");
				is.to_string_from_buffer(buf, settings, depth);
			}
		}
	}

	pub(crate) fn get_position(&self) -> Cow<Span> {
		match self {
			TypeCondition::Extends { position, .. } | TypeCondition::Is { position, .. } => {
				Cow::Borrowed(position)
			}
		}
	}
}

/// The result of a [TypeReference::Condition]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "self-rust-tokenize", derive(self_rust_tokenize::SelfRustTokenize))]
pub enum TypeConditionResult {
	/// TODO e.g. `infer number`
	Infer(Box<TypeReference>, Span),
	Reference(Box<TypeReference>),
}

impl ASTNode for TypeConditionResult {
	fn get_position(&self) -> Cow<Span> {
		match self {
			TypeConditionResult::Infer(_, pos) => Cow::Borrowed(pos),
			TypeConditionResult::Reference(reference) => reference.get_position(),
		}
	}

	fn from_reader(
		reader: &mut impl TokenReader<TSXToken, Span>,
		state: &mut crate::ParsingState,
		settings: &ParseSettings,
	) -> ParseResult<Self> {
		if matches!(reader.peek().unwrap().0, TSXToken::Keyword(TSXKeyword::Infer)) {
			let Token(_, start) = reader.next().unwrap();
			let inferred_type = TypeReference::from_reader(reader, state, settings)?;
			let position = start.union(&inferred_type.get_position());
			Ok(Self::Infer(Box::new(inferred_type), position))
		} else {
			TypeReference::from_reader(reader, state, settings)
				.map(|ty_ref| Self::Reference(Box::new(ty_ref)))
		}
	}

	fn to_string_from_buffer<T: source_map::ToString>(
		&self,
		buf: &mut T,
		settings: &crate::ToStringSettingsAndData,
		depth: u8,
	) {
		match self {
			TypeConditionResult::Infer(inferred_type, _) => {
				buf.push_str("infer ");
				inferred_type.to_string_from_buffer(buf, settings, depth);
			}
			TypeConditionResult::Reference(reference) => {
				reference.to_string_from_buffer(buf, settings, depth)
			}
		}
	}
}

impl ASTNode for TypeReference {
	fn from_reader(
		reader: &mut impl TokenReader<TSXToken, Span>,
		state: &mut crate::ParsingState,
		settings: &ParseSettings,
	) -> ParseResult<Self> {
		Self::from_reader_with_config(reader, state, settings, false)
	}

	fn to_string_from_buffer<T: source_map::ToString>(
		&self,
		buf: &mut T,
		settings: &crate::ToStringSettingsAndData,
		depth: u8,
	) {
		match self {
			Self::Cursor(..) => {
				if !settings.0.expect_cursors {
					panic!()
				}
			}
			Self::Decorated(decorator, on_type_reference, _) => {
				decorator.to_string_from_buffer(buf, settings, depth);
				buf.push(' ');
				on_type_reference.to_string_from_buffer(buf, settings, depth)
			}
			Self::Name(name, _) => buf.push_str(name),
			Self::NameWithGenericArguments(name, arguments, _) => {
				buf.push_str(name);
				to_string_bracketed(arguments, ('<', '>'), buf, settings, depth)
			}
			Self::FunctionLiteral { type_parameters, parameters, return_type, .. } => {
				if let Some(type_parameters) = type_parameters {
					to_string_bracketed(type_parameters, ('<', '>'), buf, settings, depth)
				}
				parameters.to_string_from_buffer(buf, settings, depth);
				buf.push_str(" => ");
				return_type.to_string_from_buffer(buf, settings, depth);
			}
			Self::BooleanLiteral(expression, _) => {
				buf.push_str(if *expression { "true" } else { "false" });
			}
			Self::NumberLiteral(value, _) => {
				buf.push_str(&value.to_string());
			}
			Self::StringLiteral(expression, _) => {
				buf.push('"');
				buf.push_str(expression.as_str());
				buf.push('"');
			}
			Self::Union(union_members) => {
				for (at_end, member) in union_members.iter().endiate() {
					member.to_string_from_buffer(buf, settings, depth);
					if !at_end {
						buf.push_str(" | ");
					}
				}
			}
			Self::Intersection(intersection_members) => {
				for (at_end, member) in intersection_members.iter().endiate() {
					member.to_string_from_buffer(buf, settings, depth);
					if !at_end {
						buf.push_str(" & ");
					}
				}
			}
			Self::NamespacedName(..) => unimplemented!(),
			Self::ObjectLiteral(members, _, _) => {
				buf.push('{');
				for (at_end, member) in members.iter().endiate() {
					member.to_string_from_buffer(buf, settings, depth);
					if !at_end {
						buf.push_str(", ");
					}
				}
				buf.push('}');
			}
			Self::TupleLiteral(members, _, _) => {
				buf.push('[');
				for (at_end, member) in members.iter().endiate() {
					match member {
						TupleElement::NonSpread { name, ty }
						| TupleElement::Spread { name, ty } => {
							if let Some(name) = name {
								buf.push_str(name);
								buf.push_str(": ");
							}
							if matches!(member, TupleElement::Spread { .. }) {
								buf.push_str("...");
							}
							ty.to_string_from_buffer(buf, settings, depth);
						}
					}
					if !at_end {
						buf.push_str(", ");
					}
				}
				buf.push(']');
			}

			Self::Index(..) => unimplemented!(),
			Self::KeyOf(..) => unimplemented!(),
			Self::Conditional { condition, resolve_true, resolve_false, .. } => {
				condition.to_string_from_buffer(buf, settings, depth);
				buf.push_str(" ? ");
				resolve_true.to_string_from_buffer(buf, settings, depth);
				buf.push_str(" : ");
				resolve_false.to_string_from_buffer(buf, settings, depth);
			}
			Self::ArrayLiteral(item, _) => {
				item.to_string_from_buffer(buf, settings, depth);
				buf.push_str("[]");
			}
			Self::ConstructorLiteral { parameters, type_parameters, return_type, .. } => {
				buf.push_str("new ");
				if let Some(type_parameters) = type_parameters {
					to_string_bracketed(type_parameters, ('<', '>'), buf, settings, depth);
				}
				parameters.to_string_from_buffer(buf, settings, depth);
				buf.push_str(" => ");
				return_type.to_string_from_buffer(buf, settings, depth);
			}
			Self::Readonly(readonly_type, _) => {
				buf.push_str("readonly ");
				readonly_type.to_string_from_buffer(buf, settings, depth);
			}
			Self::ParenthesizedReference(reference, _) => {
				buf.push('(');
				reference.to_string_from_buffer(buf, settings, depth);
				buf.push(')');
			}
			Self::TemplateLiteral(parts, _) => {
				buf.push('`');
				for part in parts {
					match part {
						TemplateLiteralPart::Static(chunk) => buf.push_str(chunk),
						TemplateLiteralPart::Dynamic(reference) => {
							buf.push_str("${");
							reference.to_string_from_buffer(buf, settings, depth);
							buf.push('}');
						}
					}
				}
				buf.push('`');
			}
		}
	}

	fn get_position(&self) -> Cow<Span> {
		match self {
			Self::Name(_, position)
			| Self::NamespacedName(_, _, position)
			| Self::NameWithGenericArguments(_, _, position)
			| Self::ArrayLiteral(_, position)
			| Self::BooleanLiteral(_, position)
			| Self::StringLiteral(_, position)
			| Self::NumberLiteral(_, position)
			| Self::Readonly(_, position)
			| Self::Conditional { position, .. }
			| Self::ObjectLiteral(_, _, position)
			| Self::TupleLiteral(_, _, position)
			| Self::Index(_, _, position)
			| Self::KeyOf(_, position)
			| Self::ParenthesizedReference(_, position)
			| Self::Cursor(_, position)
			| Self::TemplateLiteral(_, position)
			| Self::Decorated(_, _, position) => Cow::Borrowed(position),
			Self::FunctionLiteral { parameters, return_type, .. } => {
				Cow::Owned(parameters.get_position().union(&return_type.get_position()))
			}
			Self::ConstructorLiteral { new_keyword, return_type, .. } => {
				Cow::Owned(new_keyword.1.union(&return_type.get_position()))
			}
			Self::Intersection(items) | Self::Union(items) => Cow::Owned(
				items.first().unwrap().get_position().union(&items.last().unwrap().get_position()),
			),
		}
	}
}

impl TypeReference {
	/// Also returns the depth the generic arguments ran over
	/// TODO refactor and tidy a lot of this
	pub(crate) fn from_reader_with_config(
		reader: &mut impl TokenReader<TSXToken, Span>,
		state: &mut crate::ParsingState,
		settings: &ParseSettings,
		return_on_union_or_intersection: bool,
	) -> ParseResult<Self> {
		while let Some(Token(TSXToken::Comment(_) | TSXToken::MultiLineComment(_), _)) =
			reader.peek()
		{
			reader.next();
		}
		let mut reference = match reader.next().ok_or_else(parse_lexing_error)? {
			// Literals:
			Token(TSXToken::Keyword(TSXKeyword::True), pos) => Self::BooleanLiteral(true, pos),
			Token(TSXToken::Keyword(TSXKeyword::False), pos) => Self::BooleanLiteral(false, pos),
			Token(TSXToken::NumberLiteral(num), pos) => {
				Self::NumberLiteral(num.parse::<NumberStructure>().unwrap(), pos)
			}
			Token(TSXToken::SingleQuotedStringLiteral(content), pos)
			| Token(TSXToken::DoubleQuotedStringLiteral(content), pos) => Self::StringLiteral(content, pos),
			Token(TSXToken::At, pos) => {
				let decorator =
					Decorator::from_reader_sub_at_symbol(reader, state, settings, pos.clone())?;
				let this_declaration =
					Self::from_reader_with_config(reader, state, settings, true)?;
				let position = pos.union(&this_declaration.get_position());
				Self::Decorated(decorator, Box::new(this_declaration), position)
			}
			// Function literal or group
			Token(TSXToken::OpenParentheses, start_position) => {
				// Discern between group or arrow function:
				let mut bracket_count = 1;
				let next = reader.scan(|t, _| {
					match t {
						TSXToken::OpenParentheses => {
							bracket_count += 1;
						}
						TSXToken::CloseParentheses => {
							bracket_count -= 1;
						}
						_ => {}
					}
					bracket_count == 0
				});
				// If arrow function OR group
				if let Some(Token(TSXToken::Arrow, _)) = next {
					let parameters =
						TypeReferenceFunctionParameters::from_reader_sub_open_parenthesis(
							reader,
							state,
							settings,
							start_position,
						)?;
					reader.expect_next(TSXToken::Arrow)?;
					let return_type = Self::from_reader(reader, state, settings)?;
					Self::FunctionLiteral {
						type_parameters: None,
						parameters,
						type_id: TypeId::new(),
						return_type: Box::new(return_type),
					}
				} else {
					let type_reference = Self::from_reader(reader, state, settings)?;
					let end_position = reader.expect_next(TSXToken::CloseParentheses)?;
					let position = start_position.union(&end_position);
					Self::ParenthesizedReference(type_reference.into(), position)
				}
			}
			Token(TSXToken::OpenChevron, _start) => {
				let (type_parameters, _) =
					parse_bracketed(reader, state, settings, None, TSXToken::CloseChevron)?;
				let parameters =
					TypeReferenceFunctionParameters::from_reader(reader, state, settings)?;
				reader.expect_next(TSXToken::Arrow)?;
				let return_type = Self::from_reader(reader, state, settings)?;
				Self::FunctionLiteral {
					type_parameters: Some(type_parameters),
					parameters,
					type_id: TypeId::new(),
					return_type: Box::new(return_type),
				}
			}
			// Object literal type
			Token(TSXToken::OpenBrace, start) => {
				let members = parse_interface_members(reader, state, settings)?;
				let position = start.union(&reader.expect_next(TSXToken::CloseBrace)?);
				Self::ObjectLiteral(members, TypeId::new(), position)
			}
			// Tuple literal type
			Token(TSXToken::OpenBracket, start_pos) => {
				let mut members = Vec::new();
				loop {
					if let Some(Token(TSXToken::CloseBrace, _)) = reader.peek() {
						break;
					}
					let name = if let Some(Token(TSXToken::Colon, _)) = reader.peek_n(1) {
						let (name, _) = token_as_identifier(
							reader.next().unwrap(),
							"tuple literal named item",
						)?;
						reader.next();
						Some(name)
					} else {
						None
					};
					let member = if let Some(Token(TSXToken::Spread, _)) = reader.peek() {
						reader.next();
						let ty = TypeReference::from_reader(reader, state, settings)?;
						TupleElement::Spread { name, ty }
					} else {
						let ty = TypeReference::from_reader(reader, state, settings)?;
						TupleElement::NonSpread { name, ty }
					};
					members.push(member);
					if let Some(Token(TSXToken::Comma, _)) = reader.peek() {
						reader.next();
					} else {
						break;
					}
				}
				let end_pos = reader.expect_next(TSXToken::CloseBracket)?;
				Self::TupleLiteral(members, TypeId::new(), start_pos.union(&end_pos))
			}
			Token(TSXToken::TemplateLiteralStart, start) => {
				let mut parts = Vec::new();
				let mut end = None;
				while end.is_none() {
					match reader.next().ok_or_else(parse_lexing_error)? {
						Token(TSXToken::TemplateLiteralChunk(chunk), _) => {
							parts.push(TemplateLiteralPart::Static(chunk));
						}
						Token(TSXToken::TemplateLiteralExpressionStart, _) => {
							let expression = TypeReference::from_reader(reader, state, settings)?;
							reader.expect_next(TSXToken::TemplateLiteralExpressionEnd)?;
							parts.push(TemplateLiteralPart::Dynamic(Box::new(expression)));
						}
						Token(TSXToken::TemplateLiteralEnd, end_position) => {
							end = Some(end_position);
						}
						_ => unreachable!(),
					}
				}
				Self::TemplateLiteral(parts, start.union(&end.unwrap()))
			}
			Token(TSXToken::Keyword(TSXKeyword::Readonly), start) => {
				let readonly_type = TypeReference::from_reader(reader, state, settings)?;
				let position = start.union(&readonly_type.get_position());
				return Ok(TypeReference::Readonly(Box::new(readonly_type), position));
			}
			Token(TSXToken::Keyword(TSXKeyword::KeyOf), start) => {
				let key_of_type = TypeReference::from_reader(reader, state, settings)?;
				let position = start.union(&key_of_type.get_position());
				return Ok(TypeReference::KeyOf(Box::new(key_of_type), position));
			}
			Token(TSXToken::Keyword(TSXKeyword::New), span) => {
				let type_parameters = reader
					.conditional_next(|token| *token == TSXToken::OpenChevron)
					.is_some()
					.then(|| {
						parse_bracketed(reader, state, settings, None, TSXToken::CloseChevron)
							.map(|(params, _items)| params)
					})
					.transpose()?;
				let parameters =
					TypeReferenceFunctionParameters::from_reader(reader, state, settings)?;

				reader.expect_next(TSXToken::Arrow)?;
				let return_type = Self::from_reader(reader, state, settings)?;
				Self::ConstructorLiteral {
					new_keyword: Keyword::new(span),
					parameters,
					type_parameters,
					return_type: Box::new(return_type),
				}
			}
			token => {
				let (name, pos) = token_as_identifier(token, "type reference")?;
				Self::Name(name, pos)
			}
		};
		// Namespaced name
		if let Some(Token(TSXToken::Dot, _)) = reader.peek() {
			reader.next();
			let (name, start) =
				if let Self::Name(name, start) = reference { (name, start) } else { panic!() };
			let (namespace_member, end) =
				token_as_identifier(reader.next().unwrap(), "namespace name")?;
			let position = start.union(&end);
			return Ok(TypeReference::NamespacedName(name, namespace_member, position));
		}
		// Generics arguments:
		if let Some(Token(TSXToken::OpenChevron, _position)) = reader.peek() {
			// Assert its a Self::Name
			let (name, start_span) = if let Self::Name(name, start_span) = reference {
				(name, start_span)
			} else {
				let Token(_, position) = reader.next().unwrap();
				return Err(ParseError::new(
					crate::ParseErrors::TypeArgumentsNotValidOnReference,
					position,
				));
			};
			reader.next();
			let (generic_arguments, end_span) = generic_arguments_from_reader_sub_open_angle(
				reader,
				state,
				settings,
				return_on_union_or_intersection,
			)?;
			reference = Self::NameWithGenericArguments(
				name,
				generic_arguments,
				start_span.union(&end_span),
			);
			return Ok(reference);
		};
		// Array shorthand & indexing type references. Loops as number[][]
		// Not sure if index type can be looped
		while reader.conditional_next(|tok| *tok == TSXToken::OpenBracket).is_some() {
			let start = reference.get_position();
			if let Some(Token(TSXToken::CloseBracket, _)) = reader.peek() {
				let position = reference
					.get_position()
					.union(&reader.next().ok_or_else(parse_lexing_error)?.1);
				reference = Self::ArrayLiteral(Box::new(reference), position);
			} else {
				// E.g type allTypes = Person[keyof Person];
				let indexer = TypeReference::from_reader(reader, state, settings)?;
				let position = start.union(&reader.expect_next(TSXToken::CloseBracket)?);
				reference = Self::Index(Box::new(reference), Box::new(indexer), position);
			}
		}

		// Extends, Is, Intersections & Unions or implicit function literals
		match reader.peek() {
			Some(Token(TSXToken::Keyword(TSXKeyword::Extends), _)) => {
				reader.next();
				let extends_type =
					TypeReference::from_reader_with_config(reader, state, settings, true)?;
				// TODO depth
				let position = reference.get_position().union(&extends_type.get_position());
				let condition = TypeCondition::Extends {
					r#type: Box::new(reference),
					extends: Box::new(extends_type),
					position,
				};
				reader.expect_next(TSXToken::QuestionMark)?;
				// TODO may need to return here
				// if return_on_union_or_intersection {
				//     return Ok((reference, 0));
				// }
				let lhs = TypeConditionResult::from_reader(reader, state, settings)?;
				reader.expect_next(TSXToken::Colon)?;
				let rhs = TypeConditionResult::from_reader(reader, state, settings)?;
				let position = condition.get_position().union(&rhs.get_position());
				// TODO zero here ..?
				Ok(TypeReference::Conditional {
					condition,
					resolve_true: lhs,
					resolve_false: rhs,
					position,
				})
			}
			Some(Token(TSXToken::Keyword(TSXKeyword::Is), _)) => {
				reader.next();
				let is_type =
					TypeReference::from_reader_with_config(reader, state, settings, true)?;
				// TODO depth
				let position = reference.get_position().union(&is_type.get_position());
				let condition = TypeCondition::Is {
					r#type: Box::new(reference),
					is: Box::new(is_type),
					position,
				};
				reader.expect_next(TSXToken::QuestionMark)?;
				// TODO may need to return here
				// if return_on_union_or_intersection {
				//     return Ok((reference, 0));
				// }
				let resolve_true = TypeConditionResult::from_reader(reader, state, settings)?;
				reader.expect_next(TSXToken::Colon)?;
				let resolve_false = TypeConditionResult::from_reader(reader, state, settings)?;
				let position = condition.get_position().union(&resolve_false.get_position());
				Ok(TypeReference::Conditional { condition, resolve_true, resolve_false, position })
			}
			Some(Token(TSXToken::BitwiseOr, _)) => {
				if return_on_union_or_intersection {
					return Ok(reference);
				}
				let mut union_members = vec![reference];
				while let Some(Token(TSXToken::BitwiseOr, _)) = reader.peek() {
					reader.next();
					union_members
						.push(Self::from_reader_with_config(reader, state, settings, true)?);
				}
				Ok(Self::Union(union_members))
			}
			Some(Token(TSXToken::BitwiseAnd, _)) => {
				if return_on_union_or_intersection {
					return Ok(reference);
				}
				let mut intersection_members = vec![reference];
				while let Some(Token(TSXToken::BitwiseAnd, _)) = reader.peek() {
					reader.next();
					intersection_members
						.push(Self::from_reader_with_config(reader, state, settings, true)?);
				}
				Ok(Self::Intersection(intersection_members))
			}
			Some(Token(TSXToken::Arrow, _)) => {
				reader.next();
				let return_type = Self::from_reader_with_config(reader, state, settings, true)?;
				let position = reference.get_position().into_owned();
				let function = Self::FunctionLiteral {
					type_parameters: None,
					parameters: TypeReferenceFunctionParameters {
						parameters: vec![TypeReferenceFunctionParameter {
							name: None,
							type_reference: reference,
							decorators: Default::default(),
						}],
						optional_parameters: Default::default(),
						rest_parameter: None,
						position,
					},
					return_type: Box::new(return_type),
					type_id: TypeId::new(),
				};
				Ok(function)
			}
			_ => Ok(reference),
		}
	}
}

/// Parses the arguments (vector of [TypeReference]s) parsed to to a type reference or function call.
/// Returns arguments and the closing span.
/// TODO could use parse bracketed but needs to have the more complex logic inside
pub(crate) fn generic_arguments_from_reader_sub_open_angle(
	reader: &mut impl TokenReader<TSXToken, Span>,
	state: &mut crate::ParsingState,
	settings: &ParseSettings,
	return_on_union_or_intersection: bool,
) -> ParseResult<(Vec<TypeReference>, Span)> {
	let mut generic_arguments = Vec::new();

	loop {
		let argument = TypeReference::from_reader_with_config(
			reader,
			state,
			settings,
			return_on_union_or_intersection,
		)?;
		generic_arguments.push(argument);

		// Handling for the fact that concessive chevrons are grouped into bitwise shifts
		// One option is to keep track of depth but as a simpler way mutate the upcoming token
		// TODO spans

		let peek_mut = reader.peek_mut();

		if let Some(Token(t @ TSXToken::BitwiseShiftRight, span)) = peek_mut {
			let close_chevron_span =
				Span { start: span.start, end: span.start + 1, source_id: span.source_id };
			// Snipped
			span.start += 1;
			*t = TSXToken::CloseChevron;
			return Ok((generic_arguments, close_chevron_span));
		}

		if let Some(Token(t @ TSXToken::BitwiseShiftRightUnsigned, span)) = peek_mut {
			let close_chevron_span =
				Span { start: span.start, end: span.start + 1, source_id: span.source_id };
			// Snipped
			span.start += 1;
			*t = TSXToken::CloseChevron;
			return Ok((generic_arguments, close_chevron_span));
		}

		match reader.next().ok_or_else(parse_lexing_error)? {
			Token(TSXToken::Comma, _) => {}
			Token(TSXToken::CloseChevron, end_span) => return Ok((generic_arguments, end_span)),
			Token(token, position) => {
				return Err(ParseError::new(
					crate::ParseErrors::UnexpectedToken {
						expected: &[TSXToken::CloseChevron, TSXToken::Comma],
						found: token,
					},
					position,
				));
			}
		};
	}
}

/// Mirrors [crate::FunctionParameters]
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "self-rust-tokenize", derive(self_rust_tokenize::SelfRustTokenize))]
pub struct TypeReferenceFunctionParameters {
	pub parameters: Vec<TypeReferenceFunctionParameter>,
	pub optional_parameters: Vec<TypeReferenceFunctionParameter>,
	pub rest_parameter: Option<Box<TypeReferenceSpreadFunctionParameter>>,
	pub position: Span,
}

impl ASTNode for TypeReferenceFunctionParameters {
	fn get_position(&self) -> Cow<Span> {
		Cow::Borrowed(&self.position)
	}

	fn from_reader(
		reader: &mut impl TokenReader<TSXToken, Span>,
		state: &mut crate::ParsingState,
		settings: &ParseSettings,
	) -> ParseResult<Self> {
		let span = reader.expect_next(TSXToken::OpenParentheses)?;
		Self::from_reader_sub_open_parenthesis(reader, state, settings, span)
	}

	fn to_string_from_buffer<T: source_map::ToString>(
		&self,
		buf: &mut T,
		settings: &crate::ToStringSettingsAndData,
		depth: u8,
	) {
		for parameter in self.parameters.iter() {
			if let Some(ref name) = parameter.name {
				name.to_string_from_buffer(buf, settings, depth);
			}
			buf.push_str(": ");
			parameter.type_reference.to_string_from_buffer(buf, settings, depth);
		}
		for parameter in self.optional_parameters.iter() {
			if let Some(ref name) = parameter.name {
				name.to_string_from_buffer(buf, settings, depth);
			}
			buf.push_str("?: ");
			parameter.type_reference.to_string_from_buffer(buf, settings, depth);
		}
		if let Some(ref rest_parameter) = self.rest_parameter {
			buf.push_str("...");
			buf.push_str(&rest_parameter.name);
			rest_parameter.type_reference.to_string_from_buffer(buf, settings, depth);
		}
	}
}

impl TypeReferenceFunctionParameters {
	pub(crate) fn from_reader_sub_open_parenthesis(
		reader: &mut impl TokenReader<TSXToken, Span>,
		state: &mut crate::ParsingState,
		settings: &ParseSettings,
		open_paren_span: Span,
	) -> ParseResult<Self> {
		let mut parameters = Vec::new();
		let mut optional_parameters = Vec::new();
		let mut rest_parameter = None;
		while !matches!(reader.peek(), Some(Token(TSXToken::CloseParentheses, _))) {
			while reader.peek().map_or(false, |Token(r#type, _)| r#type.is_comment()) {
				reader.next();
			}
			let mut decorators = Vec::<Decorator>::new();
			while let Some(Token(TSXToken::At, _)) = reader.peek() {
				decorators.push(Decorator::from_reader(reader, state, settings)?);
			}
			if let Some(Token(TSXToken::Spread, _)) = reader.peek() {
				let Token(_, span) = reader.next().unwrap();
				let (name, _) = token_as_identifier(
					reader.next().ok_or_else(parse_lexing_error)?,
					"spread function parameter",
				)?;
				reader.expect_next(TSXToken::Colon)?;
				let type_reference = TypeReference::from_reader(reader, state, settings)?;
				rest_parameter = Some(Box::new(TypeReferenceSpreadFunctionParameter {
					spread_position: span,
					name,
					type_reference,
					decorators,
				}));
				break;
			} else {
				let mut depth = 0;
				let after_variable_field = reader.scan(|token, _| match token {
					TSXToken::OpenBracket | TSXToken::OpenBrace | TSXToken::OpenParentheses => {
						depth += 1;
						false
					}
					TSXToken::CloseBracket | TSXToken::CloseBrace | TSXToken::CloseParentheses => {
						depth -= 1;
						depth == 0
					}
					_ => depth == 0,
				});
				let name: Option<WithComment<VariableField<VariableFieldInTypeReference>>> =
					if let Some(Token(TSXToken::Colon | TSXToken::OptionalMember, _)) =
						after_variable_field
					{
						Some(ASTNode::from_reader(reader, state, settings)?)
					} else {
						None
					};
				let is_optional = match reader.next().ok_or_else(parse_lexing_error)? {
					Token(TSXToken::Colon, _) => false,
					Token(TSXToken::OptionalMember, _) => true,
					Token(token, position) => {
						return Err(ParseError::new(
							crate::ParseErrors::UnexpectedToken {
								expected: &[TSXToken::Colon, TSXToken::OptionalMember],
								found: token,
							},
							position,
						));
					}
				};
				let type_reference = TypeReference::from_reader(reader, state, settings)?;
				let parameter = TypeReferenceFunctionParameter { decorators, name, type_reference };
				if is_optional {
					optional_parameters.push(parameter);
				} else {
					parameters.push(parameter);
				}
			}

			if reader.conditional_next(|tok| matches!(tok, TSXToken::Comma)).is_none() {
				break;
			}
		}
		let end_span = reader.expect_next(TSXToken::CloseParentheses)?;
		Ok(TypeReferenceFunctionParameters {
			position: open_paren_span.union(&end_span),
			parameters,
			optional_parameters,
			rest_parameter,
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "self-rust-tokenize", derive(self_rust_tokenize::SelfRustTokenize))]
pub struct TypeReferenceFunctionParameter {
	pub decorators: Vec<Decorator>,
	/// Ooh nice optional
	pub name: Option<WithComment<VariableField<VariableFieldInTypeReference>>>,
	pub type_reference: TypeReference,
}

impl TypeReferenceFunctionParameter {
	// TODO decorators
	pub fn get_position(&self) -> Cow<Span> {
		if let Some(ref name) = self.name {
			Cow::Owned(name.get_position().union(&self.type_reference.get_position()))
		} else {
			self.type_reference.get_position()
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "self-rust-tokenize", derive(self_rust_tokenize::SelfRustTokenize))]
pub struct TypeReferenceSpreadFunctionParameter {
	pub decorators: Vec<Decorator>,
	pub spread_position: Span,
	pub name: String,
	pub type_reference: TypeReference,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{assert_matches_ast, span, NumberStructure};

	#[test]
	fn name() {
		assert_matches_ast!("string", TypeReference::Name(Deref @ "string", span!(0, 6)))
	}

	#[test]
	fn literals() {
		assert_matches_ast!(
			"\"string\"",
			TypeReference::StringLiteral(Deref @ "string", span!(0, 8))
		);
		assert_matches_ast!(
			"45",
			TypeReference::NumberLiteral(NumberStructure::Number(_), span!(0, 2))
		);
		assert_matches_ast!("true", TypeReference::BooleanLiteral(true, span!(0, 4)));
	}

	#[test]
	fn generics() {
		assert_matches_ast!(
			"Array<string>",
			TypeReference::NameWithGenericArguments(
				Deref @ "Array",
				Deref @ [TypeReference::Name(Deref @ "string", span!(6, 12))],
				span!(0, 13),
			)
		);

		assert_matches_ast!(
			"Map<string, number>",
			TypeReference::NameWithGenericArguments(
				Deref @ "Map",
				Deref @
				[TypeReference::Name(Deref @ "string", span!(4, 10)), TypeReference::Name(Deref @ "number", span!(12, 18))],
				span!(0, 19),
			)
		);

		assert_matches_ast!(
			"Array<Array<string>>",
			TypeReference::NameWithGenericArguments(
				Deref @ "Array",
				Deref @ [TypeReference::NameWithGenericArguments(
					Deref @ "Array",
					Deref @ [TypeReference::Name(Deref @ "string", span!(12, 18))],
					span!(6, 19),
				)],
				span!(0, 20),
			)
		);
	}

	#[test]
	fn union() {
		assert_matches_ast!(
			"string | number",
			TypeReference::Union(
				Deref @
				[TypeReference::Name(Deref @ "string", span!(0, 6)), TypeReference::Name(Deref @ "number", span!(9, 15))],
			)
		)
	}

	#[test]
	fn intersection() {
		assert_matches_ast!(
			"string & number",
			TypeReference::Intersection(
				Deref @
				[TypeReference::Name(Deref @ "string", span!(0, 6)), TypeReference::Name(Deref @ "number", span!(9, 15))],
			)
		)
	}

	#[test]
	fn tuple_literal() {
		assert_matches_ast!(
			"[number, x: string]",
			TypeReference::TupleLiteral(
				Deref @ [TupleElement::NonSpread {
					name: None,
					ty: TypeReference::Name(Deref @ "number", span!(1, 7)),
				}, TupleElement::NonSpread {
					name: Some(Deref @ "x"),
					ty: TypeReference::Name(Deref @ "string", span!(12, 18)),
				}],
				_,
				span!(0, 19),
			)
		);
	}

	#[test]
	fn functions() {
		assert_matches_ast!(
			"T => T",
			TypeReference::FunctionLiteral {
				type_parameters: None,
				parameters: TypeReferenceFunctionParameters {
					parameters: Deref @ [ TypeReferenceFunctionParameter { .. } ],
					..
				},
				return_type: Deref @ TypeReference::Name(Deref @ "T", span!(5, 6)),
				..
			}
		);
		// TODO more
	}

	#[test]
	fn template_literal() {
		assert_matches_ast!(
			"`test-${X}`",
			TypeReference::TemplateLiteral(
				Deref
				@ [TemplateLiteralPart::Static(Deref @ "test-"), TemplateLiteralPart::Dynamic(
					Deref @ TypeReference::Name(Deref @ "X", span!(8, 9)),
				)],
				_,
			)
		);
	}

	#[test]
	fn array_shorthand() {
		assert_matches_ast!(
			"string[]",
			TypeReference::ArrayLiteral(
				Deref @ TypeReference::Name(Deref @ "string", span!(0, 6)),
				span!(0, 8),
			)
		);
		assert_matches_ast!(
			"(number | null)[]",
			TypeReference::ArrayLiteral(
				Deref @ TypeReference::ParenthesizedReference(
					Deref @ TypeReference::Union(
						Deref @
						[TypeReference::Name(Deref @ "number", span!(1, 7)), TypeReference::Name(Deref @ "null", span!(10, 14))],
					),
					span!(0, 15),
				),
				span!(0, 17),
			)
		);
		assert_matches_ast!(
			"string[][]",
			TypeReference::ArrayLiteral(
				Deref @ TypeReference::ArrayLiteral(
					Deref @ TypeReference::Name(Deref @ "string", span!(0, 6)),
					span!(0, 8),
				),
				span!(0, 10),
			)
		);
	}
}
