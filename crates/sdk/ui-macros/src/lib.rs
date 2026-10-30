//! Procedural macros for `tilo-ui`. See `tilo_ui::component` for the API; this crate only
//! generates calls into it.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::quote;
use syn::spanned::Spanned;
use syn::{
    Attribute, Data, DeriveInput, Expr, ExprLit, Fields, Lit, LitStr, Meta, parse_macro_input,
};

/// Derive `tilo_ui::component::Component` for a widget's props: the struct an instance of the
/// widget is made from. The widget's component description is generated from it, so the
/// description cannot drift from the widget.
///
/// Struct attribute: `#[component(name = "ListRow", role = "ListItem")]`, with optional
/// `label = "prop"` (the prop that names it to assistive technology) and
/// `presents = "prop"` (the prop holding the resource it shows). The struct's doc text is the
/// component's.
///
/// Each field is one part of the description, with its doc text:
///
/// - no attribute, or `#[component(required)]`, `#[component(positional)]`: a **prop**, whose
///   type implements `PropType`;
/// - `#[component(slot)]`, `#[component(slot(default))]`: a **slot**, whose type implements
///   `SlotType` (`Option<Element>`, `Vec<Element>`, or `Option<P>` of another component's props:
///   the slot accepts only that component);
/// - `#[component(event)]`, `#[component(event(value = T))]`: an **event**, of type `Event`;
///   `event(value = T, at = i64)` also says where the value lands (a drop's place);
/// - `#[component(bind)]`, `#[component(bind(merge = "splice"))]`: a **binding**, of type
///   `Bound<T>`;
/// - `#[component(state)]`: the widget's own **state** (lens state) to restore, a `PropType`;
/// - `#[component(key)]`: the instance's stable key, a `String`, not described;
/// - `#[component(skip)]`: Rust only, not described.
///
/// A field of any other type does not compile: an event is data (`Event`), never a closure.
#[proc_macro_derive(Component, attributes(component))]
pub fn derive_component(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Say of a public widget why it is not a described component, where it is declared:
/// `#[not_a_component(layout, "the language's `stack`")]`. The toolkit's coverage check reads
/// it: every public widget is either described (`impl Described`) or says why not, so a widget
/// that works in Tilo is either in tilolang or names the reason it never will be.
///
/// The kinds:
///
/// - `layout`: a layout the language's own primitive builds (name it);
/// - `shell`: window management the system owns, never inside an app's window;
/// - `system`: a surface only the system may draw (a prompt for a secret or a permission, the
///   palette, system search), which an app must not be able to forge;
/// - `part`: a part of another component, reached through it (name it);
/// - `framework`: behaviour the framework applies around a view, not something a view places.
///
/// The item is left as it is.
#[proc_macro_attribute]
pub fn not_a_component(attr: TokenStream, item: TokenStream) -> TokenStream {
    match exemption(attr.into()) {
        Ok(()) => item,
        Err(error) => {
            let mut tokens: Tokens = error.to_compile_error();
            tokens.extend(Tokens::from(item));
            tokens.into()
        }
    }
}

/// The kinds `not_a_component` takes.
const EXEMPTIONS: [&str; 5] = ["layout", "shell", "system", "part", "framework"];

fn exemption(attr: Tokens) -> syn::Result<()> {
    let parser = |input: syn::parse::ParseStream<'_>| -> syn::Result<()> {
        let kind: syn::Ident = input.parse()?;
        if !EXEMPTIONS.iter().any(|k| kind == k) {
            return Err(syn::Error::new(
                kind.span(),
                format!(
                    "a widget is not a component for one of: {}",
                    EXEMPTIONS.join(", ")
                ),
            ));
        }
        input.parse::<syn::Token![,]>()?;
        let reason: LitStr = input.parse()?;
        if reason.value().trim().is_empty() {
            return Err(syn::Error::new(
                reason.span(),
                "say why it is not a component",
            ));
        }
        Ok(())
    };
    syn::parse::Parser::parse2(parser, attr)
}

/// The struct attribute.
#[derive(Default)]
struct Head {
    name: Option<String>,
    role: Option<String>,
    label: Option<String>,
    presents: Option<String>,
    /// It draws nothing where it stands and attaches to the view it stands in.
    attaches: bool,
}

/// What a field is.
enum Part {
    Prop {
        required: bool,
        positional: bool,
    },
    Slot {
        default: bool,
    },
    Event {
        value: Option<Box<syn::Type>>,
        at: Option<Box<syn::Type>>,
    },
    Bind {
        merge: Tokens,
    },
    State,
    /// The instance's key.
    Key,
    Skip,
}

fn doc_of(attrs: &[Attribute]) -> String {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        })
        .map(|line| line.trim().to_owned())
        .collect();
    // A paragraph is one line of the description; paragraphs are separated by a line break.
    let mut doc = String::new();
    for line in lines {
        if line.is_empty() {
            if !doc.is_empty() && !doc.ends_with('\n') {
                doc.push('\n');
            }
        } else {
            if !doc.is_empty() && !doc.ends_with('\n') {
                doc.push(' ');
            }
            doc.push_str(&line);
        }
    }
    doc.trim().to_owned()
}

fn head_of(attrs: &[Attribute]) -> syn::Result<Head> {
    let mut head = Head::default();
    for attr in attrs.iter().filter(|a| a.path().is_ident("component")) {
        attr.parse_nested_meta(|meta| {
            let value = || -> syn::Result<String> { Ok(meta.value()?.parse::<LitStr>()?.value()) };
            if meta.path.is_ident("name") {
                head.name = Some(value()?);
            } else if meta.path.is_ident("role") {
                head.role = Some(value()?);
            } else if meta.path.is_ident("label") {
                head.label = Some(value()?);
            } else if meta.path.is_ident("presents") {
                head.presents = Some(value()?);
            } else if meta.path.is_ident("attaches") {
                head.attaches = true;
            } else {
                return Err(meta.error(
                    "a component takes `name = \"..\"`, `role = \"..\"`, `label = \"..\"`, `presents = \"..\"` and `attaches`",
                ));
            }
            Ok(())
        })?;
    }
    Ok(head)
}

/// `event`, `event(value = T)` or `event(value = T, at = U)`: an event, what it carries and
/// where that lands.
fn event_part(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<Part> {
    let (mut value, mut at) = (None, None);
    if meta.input.peek(syn::token::Paren) {
        meta.parse_nested_meta(|inner| {
            if inner.path.is_ident("value") {
                value = Some(Box::new(inner.value()?.parse::<syn::Type>()?));
                Ok(())
            } else if inner.path.is_ident("at") {
                at = Some(Box::new(inner.value()?.parse::<syn::Type>()?));
                Ok(())
            } else {
                Err(inner.error("an event takes `value = Type` and `at = Type`"))
            }
        })?;
    }
    Ok(Part::Event { value, at })
}

fn part_of(field: &syn::Field) -> syn::Result<Part> {
    let mut part = None;
    let mut required = false;
    let mut positional = false;
    let set = |part: &mut Option<Part>, new: Part, span: proc_macro2::Span| {
        if part.is_some() {
            return Err(syn::Error::new(span, "a field is one part of a component"));
        }
        *part = Some(new);
        Ok(())
    };
    for attr in field
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("component"))
    {
        attr.parse_nested_meta(|meta| {
            let span = meta.path.span();
            if meta.path.is_ident("required") {
                required = true;
            } else if meta.path.is_ident("positional") {
                positional = true;
            } else if meta.path.is_ident("slot") {
                let mut default = false;
                if meta.input.peek(syn::token::Paren) {
                    meta.parse_nested_meta(|inner| {
                        if inner.path.is_ident("default") {
                            default = true;
                            Ok(())
                        } else {
                            Err(inner.error("a slot takes `default`"))
                        }
                    })?;
                }
                set(&mut part, Part::Slot { default }, span)?;
            } else if meta.path.is_ident("event") {
                set(&mut part, event_part(&meta)?, span)?;
            } else if meta.path.is_ident("bind") {
                let mut merge = quote!(Register);
                if meta.input.peek(syn::token::Paren) {
                    meta.parse_nested_meta(|inner| {
                        if inner.path.is_ident("merge") {
                            let how = inner.value()?.parse::<LitStr>()?;
                            merge = match how.value().as_str() {
                                "splice" => quote!(Splice),
                                "register" => quote!(Register),
                                "rich" => quote!(Rich),
                                _ => {
                                    return Err(syn::Error::new(
                                        how.span(),
                                        "a binding merges by \"splice\", \"rich\" or \"register\"",
                                    ));
                                }
                            };
                            Ok(())
                        } else {
                            Err(inner.error("a binding takes `merge = \"splice\"`"))
                        }
                    })?;
                }
                set(&mut part, Part::Bind { merge }, span)?;
            } else if meta.path.is_ident("state") {
                set(&mut part, Part::State, span)?;
            } else if meta.path.is_ident("key") {
                set(&mut part, Part::Key, span)?;
            } else if meta.path.is_ident("skip") {
                set(&mut part, Part::Skip, span)?;
            } else {
                return Err(meta.error(
                    "a component field is `required`, `positional`, `slot`, `event`, `bind`, `state`, `key` or `skip`",
                ));
            }
            Ok(())
        })?;
    }
    match part {
        Some(other) if required || positional => Err(syn::Error::new(
            field.span(),
            match other {
                Part::Skip | Part::Key => "a skipped field or the key is not a prop",
                _ => "only a prop is `required` or `positional`",
            },
        )),
        Some(part) => Ok(part),
        None => Ok(Part::Prop {
            required,
            positional,
        }),
    }
}

/// The struct attribute, the component's name and role, and its doc text.
fn described(input: &DeriveInput) -> syn::Result<(Head, String, String, String)> {
    let head = head_of(&input.attrs)?;
    let Some(name) = head.name.clone() else {
        return Err(syn::Error::new(
            input.span(),
            "name the component: `#[component(name = \"ListRow\", role = \"ListItem\")]`",
        ));
    };
    let Some(role) = head.role.clone() else {
        return Err(syn::Error::new(
            input.span(),
            "give the component's accessibility role: `#[component(role = \"ListItem\")]`",
        ));
    };
    let doc = doc_of(&input.attrs);
    if doc.is_empty() {
        return Err(syn::Error::new(
            input.span(),
            "a component's doc text is what tilolang shows for it: document the struct",
        ));
    }
    Ok((head, name, role, doc))
}

fn expand(input: &DeriveInput) -> syn::Result<Tokens> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.span(),
            "a component's props are a struct with named fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new(
            input.span(),
            "a component's props are a struct with named fields",
        ));
    };
    let (head, name, role, doc) = described(input)?;
    let ident = &input.ident;
    let k = quote!(::tilo_ui::component);
    let mut out = Generated::default();
    for field in &fields.named {
        out.field(field, &k, &name)?;
    }
    let Generated {
        props,
        slots,
        events,
        bindings,
        states,
        reads,
        known,
    } = out;
    for (what, prop) in [("label", &head.label), ("presents", &head.presents)] {
        if let Some(prop) = prop
            && !known.contains(prop)
        {
            return Err(syn::Error::new(
                input.span(),
                format!("`{what} = \"{prop}\"` names no part of the component"),
            ));
        }
    }
    let label = option_tokens(head.label.as_deref());
    let presents = option_tokens(head.presents.as_deref());
    let attaches = head.attaches;
    // A component of no parts reads nothing into its props.
    let binding = if reads.is_empty() {
        quote!(let _ = source; let props)
    } else {
        quote!(let mut props)
    };
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics #k::Component for #ident #ty_generics #where_clause {
            const NAME: &'static str = #name;

            fn describe() -> #k::Description {
                #k::Description {
                    name: #name.into(),
                    doc: #doc.into(),
                    props: vec![#(#props),*],
                    slots: vec![#(#slots),*],
                    events: vec![#(#events),*],
                    bindings: vec![#(#bindings),*],
                    state: vec![#(#states),*],
                    semantics: #k::Semantics {
                        role: #role.into(),
                        label: #label,
                        presents: #presents,
                        attaches: #attaches,
                    },
                }
            }

            fn from_source(source: &mut dyn #k::Source) -> ::core::result::Result<Self, #k::Error> {
                #binding = <Self as ::core::default::Default>::default();
                #(#reads)*
                Ok(props)
            }
        }

        /// A slot that accepts only this component reads its props.
        impl #impl_generics #k::SlotType for ::core::option::Option<#ident #ty_generics> #where_clause {
            const MANY: bool = false;

            fn accepts() -> #k::Accepts {
                #k::Accepts::Component(#name.into())
            }

            fn fill(
                &mut self,
                source: &mut dyn #k::Source,
                slot: &str,
            ) -> ::core::result::Result<(), ::std::string::String> {
                let mut read = None;
                let count = source.components(slot, &mut |child| {
                    read = Some(<#ident #ty_generics as #k::Component>::from_source(child)
                        .map_err(|error| error.to_string())?);
                    Ok(())
                })?;
                if count > 1 {
                    return Err(format!("holds one {}, not {count}", #name));
                }
                *self = read;
                Ok(())
            }
        }
    })
}

/// What the fields of a props struct generate: each part of the description, and how an
/// instance's value of it is read.
#[derive(Default)]
struct Generated {
    props: Vec<Tokens>,
    slots: Vec<Tokens>,
    events: Vec<Tokens>,
    bindings: Vec<Tokens>,
    states: Vec<Tokens>,
    reads: Vec<Tokens>,
    /// The names of the described parts.
    known: Vec<String>,
}

impl Generated {
    fn field(&mut self, field: &syn::Field, k: &Tokens, name: &str) -> syn::Result<()> {
        let Some(member) = &field.ident else {
            return Ok(());
        };
        let part = part_of(field)?;
        if self.undescribed(&part, member) {
            return Ok(());
        }
        let ty = &field.ty;
        let key = member.to_string();
        let key = key.strip_prefix("r#").unwrap_or(&key).to_owned();
        let field_doc = doc_of(&field.attrs);
        if field_doc.is_empty() {
            return Err(syn::Error::new(
                field.span(),
                "every part of a component is documented: its doc text is what tilolang shows",
            ));
        }
        self.known.push(key.clone());
        match part {
            Part::Prop {
                required,
                positional,
            } => self.prop(
                k,
                name,
                member,
                ty,
                &key,
                &field_doc,
                (required, positional),
            ),
            Part::Slot { default } => {
                self.slots.push(quote! {
                    #k::Slot {
                        name: #key.into(),
                        doc: #field_doc.into(),
                        accepts: <#ty as #k::SlotType>::accepts(),
                        many: <#ty as #k::SlotType>::MANY,
                        default: #default,
                    }
                });
                self.reads.push(quote! {
                    <#ty as #k::SlotType>::fill(&mut props.#member, source, #key)
                        .map_err(|message| #k::Error::value(#name, #key, message))?;
                });
            }
            Part::Event { value, at } => {
                let ty = |t: Option<Box<syn::Type>>| {
                    t.map_or_else(
                        || quote!(None),
                        |v| quote!(Some(<#v as #k::PropType>::ty())),
                    )
                };
                let (value, at) = (ty(value), ty(at));
                self.events.push(quote! {
                    #k::EventDescription {
                        name: #key.into(),
                        doc: #field_doc.into(),
                        value: #value,
                        at: #at,
                    }
                });
                // An event is data: the field is an `Event`, never a closure.
                self.reads.push(quote! {
                    props.#member = source.event(#key);
                });
            }
            Part::Bind { merge } => {
                self.bindings.push(quote! {
                    #k::BindingDescription {
                        name: #key.into(),
                        doc: #field_doc.into(),
                        ty: <#ty as #k::BindType>::ty(),
                        merge: #k::Merge::#merge,
                    }
                });
                self.reads.push(quote! {
                    if let Some(bound) = source.binding(#key) {
                        props.#member = <#ty as #k::BindType>::from_bound(bound)
                            .map_err(|message| #k::Error::value(#name, #key, message))?;
                    }
                });
            }
            Part::State => {
                self.states.push(quote! {
                    #k::StateDescription {
                        name: #key.into(),
                        doc: #field_doc.into(),
                        ty: <#ty as #k::PropType>::ty(),
                    }
                });
                self.reads.push(quote! {
                    if let Some(value) = source.state(#key) {
                        props.#member = <#ty as #k::PropType>::from_value(&value)
                            .map_err(|message| #k::Error::value(#name, #key, message))?;
                    }
                });
            }
            Part::Key | Part::Skip => {}
        }
        Ok(())
    }

    /// A field that is not part of the description: skipped, or the instance's key.
    fn undescribed(&mut self, part: &Part, member: &syn::Ident) -> bool {
        match part {
            Part::Skip => true,
            Part::Key => {
                self.reads.push(quote! {
                    props.#member = ::std::string::ToString::to_string(source.key());
                });
                true
            }
            _ => false,
        }
    }

    fn prop(
        &mut self,
        k: &Tokens,
        name: &str,
        member: &syn::Ident,
        ty: &syn::Type,
        key: &str,
        doc: &str,
        (required, positional): (bool, bool),
    ) {
        self.props.push(quote! {
            #k::Prop {
                name: #key.into(),
                doc: #doc.into(),
                ty: <#ty as #k::PropType>::ty(),
                required: #required,
                positional: #positional,
            }
        });
        let missing = if required {
            quote!({ return Err(#k::Error::missing(#name, #key)); })
        } else {
            quote!({})
        };
        self.reads.push(quote! {
            match source.prop(#key) {
                Some(value) => {
                    props.#member = <#ty as #k::PropType>::read(&value, source)
                        .map_err(|message| #k::Error::value(#name, #key, message))?;
                }
                None => #missing
            }
        });
    }
}

fn option_tokens(value: Option<&str>) -> Tokens {
    value.map_or_else(|| quote!(None), |v| quote!(Some(#v.into())))
}
