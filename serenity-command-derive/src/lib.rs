use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::quote;
use syn::{
    parse_macro_input, Attribute, Data, DeriveInput, Fields, GenericArgument, Lit, Meta,
    NestedMeta, PathArguments, Type,
};

struct Attr {
    key: String,
    value: String,
}

struct CommandOption {
    name: String,
    required: bool,
    autocomplete: bool,
    getter: proc_macro2::TokenStream,
    kind: proc_macro2::TokenStream,
    description: String,
}

fn get_attr_value(attrs: &[Attr], name: &str) -> syn::Result<Option<String>> {
    Ok(attrs
        .iter()
        .find(|a| a.key == name)
        .map(|a| a.value.clone()))
}

fn get_attr_list(attrs: &[Attribute]) -> Option<Vec<Attr>> {
    match attrs
        .iter()
        .find(|a| a.path.is_ident("cmd"))?
        .parse_meta()
        .unwrap()
    {
        Meta::List(list) => Some(
            list.nested
                .into_iter()
                .filter_map(|attr| match attr {
                    NestedMeta::Meta(Meta::NameValue(nv)) => {
                        let ident = nv.path.get_ident().unwrap();
                        let key = ident.to_string();
                        let value = match nv.lit {
                            Lit::Str(s) => s.value(),
                            _ => String::new(),
                        };
                        Some(Attr { key, value })
                    }
                    NestedMeta::Meta(Meta::Path(p)) => {
                        let ident = p.get_ident().unwrap();
                        let key = ident.to_string();
                        Some(Attr {
                            key,
                            value: String::new(),
                        })
                    }
                    _ => None,
                })
                .collect::<Vec<_>>(),
        ),
        _ => None,
    }
}

fn analyze_field(
    ident: &syn::Ident,
    mut ty: &Type,
    attrs: &[Attribute],
) -> syn::Result<CommandOption> {
    let attrs = get_attr_list(attrs).unwrap_or_default();
    let name = get_attr_value(&attrs, "name")?.unwrap_or_else(|| ident.to_string());
    let desc = get_attr_value(&attrs, "desc")?.unwrap_or_else(|| ident.to_string());
    let find_opt =
        quote!(opts.options.iter().find(|o| o.name == #name).and_then(|o| o.resolved.as_ref()));
    let opt_value = quote!(
        serenity::model::application::interaction::application_command::CommandDataOptionValue
    );
    let mut required = true;
    let autocomplete = get_attr_value(&attrs, "autocomplete")?.is_some();
    if let Type::Path(path) = ty {
        let segs = &path.path.segments;
        if segs.len() == 1 && segs[0].ident == "Option" {
            required = false;
            if let PathArguments::AngleBracketed(args) = &segs[0].arguments {
                ty = match &args.args[0] {
                    GenericArgument::Type(ty) => ty,
                    _ => return Err(syn::Error::new(ident.span(), "Invalid option")),
                };
            }
        }
    }
    match ty {
        Type::Path(path) => {
            let segs = &path.path.segments;
            let parts = segs
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            if segs.len() == 1 {
                let (matcher, kind) = match parts.as_str() {
                    "String" | "std::str::String" => (
                        quote!(#opt_value::String(v)),
                        quote!(serenity::model::application::command::CommandOptionType::String),
                    ),
                    "i64" => (
                        quote!(#opt_value::Integer(v)),
                        quote!(serenity::model::application::command::CommandOptionType::Integer),
                    ),
                    "f64" => (
                        quote!(#opt_value::Number(v)),
                        quote!(serenity::model::application::command::CommandOptionType::Number),
                    ),
                    "bool" => (
                        quote!(#opt_value::Boolean(v)),
                        quote!(serenity::model::application::command::CommandOptionType::Boolean),
                    ),
                    "Role" | "serenity::model::guild::Role" => (
                        quote!(#opt_value::Role(v)),
                        quote!(serenity::model::application::command::CommandOptionType::Role),
                    ),
                    "User" | "serenity::model::User" => (
                        quote!(#opt_value::User(v, _)),
                        quote!(serenity::model::application::command::CommandOptionType::User),
                    ),
                    other => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!("Unsupported type {other}"),
                        ))
                    }
                };
                let getter = if required {
                    quote!(if let Some(#matcher) = #find_opt {
                        v.clone()
                    } else {
                        panic!("Value is required")
                    })
                } else {
                    quote!(if let Some(#matcher) = #find_opt {
                        Some(v.clone())
                    } else {
                        None
                    })
                };
                return Ok(CommandOption {
                    name: ident.to_string(),
                    required,
                    autocomplete,
                    getter,
                    kind,
                    description: desc,
                });
            }
            todo!()
        }
        _ => Err(syn::Error::new(ident.span(), "Unsupported type")),
    }
}

impl CommandOption {
    fn create(&self) -> proc_macro2::TokenStream {
        let name = &self.name;
        let desc = &self.description;
        let kind = &self.kind;
        let required = self.required;
        let autocomplete = self.autocomplete;
        quote!(.create_option(|opt|{
            opt.name(#name)
                .description(#desc)
                .kind(#kind)
                .required(#required)
                .set_autocomplete(#autocomplete);
            (&extras)(#name, opt);
            opt
        }))
    }
}

fn derive(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let DeriveInput {
        ident,
        generics,
        data,
        attrs,
        ..
    } = input;
    if !generics.params.is_empty() {
        return Err(syn::Error::new(
            ident.span(),
            "Generic structs are not supported",
        ));
    }
    let attrs = get_attr_list(&attrs).unwrap_or_default();
    let s = match data {
        Data::Struct(s) => s,
        _ => {
            return Err(syn::Error::new(
                ident.span(),
                "Derive target must be a struct",
            ))
        }
    };
    let fields = match s.fields {
        Fields::Named(f) => f,
        _ => {
            return Err(syn::Error::new(
                ident.span(),
                "Derive target must use named fields",
            ))
        }
    };
    let field_names = fields.named.iter().flat_map(|f| f.ident.as_ref());
    let attr_name = get_attr_value(&attrs, "name")?;
    let name = attr_name.unwrap_or_else(|| ident.to_string());
    let desc = get_attr_value(&attrs, "desc")?.unwrap_or_else(|| ident.to_string());
    let opts: Vec<_> = fields
        .named
        .iter()
        .map(|f| analyze_field(f.ident.as_ref().unwrap(), &f.ty, &f.attrs))
        .collect::<syn::Result<_>>()?;
    let builders = opts.iter().map(CommandOption::create);
    let getters = opts.iter().map(|o| &o.getter);
    let runner_ident = Ident::new(&format!("__{}_runner", &ident), Span::call_site());
    let app_command = quote!(serenity::model::application::interaction::application_command);
    let data_ident = quote!(<#ident as serenity_command::BotCommand>::Data);
    Ok(quote!(
            impl<'a> From<&'a #app_command::CommandData> for #ident {
                fn from(opts: &'a #app_command::CommandData) -> Self {
                    #ident {
                        #(#field_names: #getters),*
                    }
                }
            }

            struct #runner_ident;

            #[async_trait]
            impl serenity_command::CommandRunner<#data_ident> for #runner_ident {
                async fn run(
                    &self,
                    data: &#data_ident,
                    ctx: &serenity::prelude::Context,
                    interaction: &#app_command::ApplicationCommandInteraction,
                    ) -> anyhow::Result<serenity_command::CommandResponse> {
                    #ident::from(&interaction.data).run(data, ctx, interaction).await
                }

                fn name(&self) -> &'static str {
                    #name
                }

                fn register<'a>(&self, builder: &'a mut serenity::builder::CreateApplicationCommand) -> &'a mut serenity::builder::CreateApplicationCommand {
                    use serenity_command::CommandBuilder;
                    #ident::create_extras(builder, <#ident as serenity_command::BotCommand>::setup_options);
                    if !#ident::PERMISSIONS.is_empty() {
                        builder.default_member_permissions(#ident::PERMISSIONS);
                    }
                    builder
                }
            }

        impl<'a> serenity_command::CommandBuilder<'a> for #ident {
        fn create_extras<E: Fn(&'static str, &mut serenity::builder::CreateApplicationCommandOption)>(
            builder: &mut serenity::builder::CreateApplicationCommand,
            extras: E
        ) -> &mut serenity::builder::CreateApplicationCommand {
            builder.name(#name)
                .description(#desc)
            #(#builders)*
        }

        fn create(builder: &mut serenity::builder::CreateApplicationCommand)
            -> &mut serenity::builder::CreateApplicationCommand
        {
            let extras = |_: &'static str, _: &mut serenity::builder::CreateApplicationCommandOption| {};
            Self::create_extras(builder, extras)
        }

        const NAME: &'static str = #name;

        fn runner() -> Box<dyn serenity_command::CommandRunner<Self::Data> + Send + Sync> {
            Box::new(#runner_ident)
        }
    }))
}

#[proc_macro_derive(Command, attributes(cmd))]
pub fn derive_serenity_command(input: TokenStream) -> TokenStream {
    derive(parse_macro_input!(input))
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}
