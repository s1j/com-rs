extern crate proc_macro;
use proc_macro::TokenStream;
type HelperTokenStream = proc_macro2::TokenStream;
use quote::{format_ident, quote,};
use syn:: {
    ItemStruct, Ident, Meta, NestedMeta, Fields,
};

use std::iter::FromIterator;
use std::collections::HashMap;
use macro_utils::camel_to_snake;

// Helper functions (CURRENTLY DUPLICATED TO MOVE DURING REBASE)

pub fn get_vtable_ident(trait_ident: &Ident) -> Ident {
    format_ident!("{}VTable", trait_ident)
}

pub fn get_vptr_ident(trait_ident: &Ident) -> Ident {
    format_ident!("{}VPtr", trait_ident)
}

fn get_ref_count_ident() -> Ident {
    format_ident!("__refcnt")
}

fn get_vptr_field_ident(trait_ident: &Ident) -> Ident {
    format_ident!("__{}vptr", trait_ident.to_string().to_lowercase())
}

fn get_real_ident(struct_ident: &Ident) -> Ident {
    if !struct_ident.to_string().starts_with("Init") {
        panic!("The target struct's name must begin with Init")
    }

    format_ident!("{}", &struct_ident.to_string()[4..])
}

fn get_inner_init_field_ident() -> Ident {
    format_ident!("__init_struct")
}

fn get_base_interface_idents(struct_item: &ItemStruct) -> Vec<Ident> {
    let mut base_itf_idents = Vec::new();

    for attr in &struct_item.attrs {
        if let Ok(Meta::List(ref attr)) = attr.parse_meta() {
            if attr.path.segments.last().unwrap().ident != "com_implements" {
                continue;
            }

            for item in &attr.nested {
                if let NestedMeta::Meta(Meta::Path(p)) = item {
                    assert!(p.segments.len() == 1, "Incapable of handling multiple path segments yet.");
                    base_itf_idents.push(p.segments.last().unwrap().ident.clone());
                }
            }
        }
    }

    base_itf_idents
}

fn get_aggr_map(struct_item: &ItemStruct) -> HashMap<Ident, Vec<Ident>> {
    let mut aggr_map = HashMap::new();

    let fields = match &struct_item.fields {
        Fields::Named(f) => &f.named,
        _ => panic!("Found field other than named fields in struct")
    };

    for field in fields {
        for attr in &field.attrs {
            if let Ok(Meta::List(ref attr)) = attr.parse_meta() {
                if attr.path.segments.last().unwrap().ident != "aggr" {
                    continue;
                }

                let mut aggr_interfaces_idents = Vec::new();


                assert!(attr.nested.len() > 0, "Need to expose at least one interface from aggregated COM object.");

                for item in &attr.nested {
                    if let NestedMeta::Meta(Meta::Path(p)) = item {
                        assert!(p.segments.len() == 1, "Incapable of handling multiple path segments yet.");
                        aggr_interfaces_idents.push(p.segments.last().unwrap().ident.clone());
                    }
                }
                let ident = field.ident.as_ref().unwrap().clone();
                aggr_map.insert(ident, aggr_interfaces_idents);
            }
        }
    }

    aggr_map
}

// Macro expansion entry point.

pub fn expand_derive_com_class(item: TokenStream) -> TokenStream {

    let input = syn::parse_macro_input!(item as ItemStruct);

    // Parse attributes
    let base_itf_idents = get_base_interface_idents(&input);
    let aggr_itf_idents = get_aggr_map(&input);

    let mut out: Vec<TokenStream> = Vec::new();
    out.push(gen_real_struct(&base_itf_idents, &input).into());
    out.push(gen_allocate_impl(&base_itf_idents, &input).into());
    out.push(gen_iunknown_impl(&base_itf_idents, &aggr_itf_idents, &input).into());
    out.push(gen_drop_impl(&base_itf_idents, &input).into());
    out.push(gen_deref_impl(&input).into());

    let out = TokenStream::from_iter(out);
    println!("Result:\n{}", out);
    out
}

fn gen_drop_impl(base_itf_idents: &[Ident], struct_item: &ItemStruct) -> HelperTokenStream {
    let real_ident = get_real_ident(&struct_item.ident);
    let box_from_raws = base_itf_idents.iter().map(|base| {
        let vptr_field_ident = get_vptr_field_ident(&base);
        let vtable_ident = get_vtable_ident(&base);
        quote!(
            Box::from_raw(self.#vptr_field_ident as *mut #vtable_ident);
        )
    });

    quote!(
        impl std::ops::Drop for #real_ident {
            fn drop(&mut self) {
                let _ = unsafe {
                    #(#box_from_raws)*
                };
            }
        }
    )
}

fn gen_deref_impl(struct_item: &ItemStruct) -> HelperTokenStream {
    let init_ident = &struct_item.ident;
    let real_ident = get_real_ident(init_ident);
    let inner_init_field_ident = get_inner_init_field_ident();

    quote!(
        impl std::ops::Deref for #real_ident {
            type Target = #init_ident;
            fn deref(&self) -> &Self::Target {
                &self.#inner_init_field_ident
            }
        }
    )
}

fn gen_iunknown_impl(base_itf_idents: &[Ident], aggr_itf_idents: &HashMap<Ident, Vec<Ident>>, struct_item: &ItemStruct) -> HelperTokenStream {
    let real_ident = get_real_ident(&struct_item.ident);
    let ref_count_ident = get_ref_count_ident();

    let first_vptr_field = get_vptr_field_ident(&base_itf_idents[0]);

    // Generate match arms for implemented interfaces
    let base_match_arms = base_itf_idents.iter().map(|base| {
        let match_condition = quote!(<dyn #base as com::ComInterface>::iid_in_inheritance_chain(riid));
        let vptr_field_ident = get_vptr_field_ident(&base);

        quote!(
            else if #match_condition {
                *ppv = &self.#vptr_field_ident as *const _ as *mut c_void;
            }
        )
    });

    // Generate match arms for aggregated interfaces
    let aggr_match_arms = aggr_itf_idents.iter().map(|(aggr_field_ident, aggr_base_itf_idents)| {

        // Construct the OR match conditions for a single aggregated object.
        let first_base_itf_ident = &aggr_base_itf_idents[0];
        let first_aggr_match_condition = quote!(
            <dyn #first_base_itf_ident as com::ComInterface>::iid_in_inheritance_chain(riid)
        );
        let rem_aggr_match_conditions = aggr_base_itf_idents.iter().skip(1).map(|base| {
            quote!(|| <dyn #base as com::ComInterface>::iid_in_inheritance_chain(riid))
        });

        quote!(
            else if #first_aggr_match_condition #(#rem_aggr_match_conditions)* {
                let mut aggr_itf_ptr: ComPtr<dyn IUnknown> = ComPtr::new(self.#aggr_field_ident as *mut c_void);
                let hr = aggr_itf_ptr.query_interface(riid, ppv);
                if com::failed(hr) {
                    return winapi::shared::winerror::E_NOINTERFACE;
                }

                // We release it as the previous call add_ref-ed the inner object.
                // The intention is to transfer reference counting logic to the
                // outer object.
                aggr_itf_ptr.release();

                forget(aggr_itf_ptr);
            }
        )
    });

    quote!(
        impl com::IUnknown for #real_ident {
            fn query_interface(
                &mut self,
                riid: *const winapi::shared::guiddef::IID,
                ppv: *mut *mut winapi::ctypes::c_void
            ) -> winapi::shared::winerror::HRESULT {
                unsafe {
                    let riid = &*riid;

                    if winapi::shared::guiddef::IsEqualGUID(riid, &com::IID_IUNKNOWN) {
                        *ppv = &self.#first_vptr_field as *const _ as *mut c_void;
                    } #(#base_match_arms)* #(#aggr_match_arms)* else {
                        *ppv = std::ptr::null_mut::<winapi::ctypes::c_void>();
                        println!("Returning NO INTERFACE.");
                        return winapi::shared::winerror::E_NOINTERFACE;
                    }

                    println!("Successful!.");
                    self.add_ref();
                    NOERROR
                }
            }

            fn add_ref(&mut self) -> u32 {
                self.#ref_count_ident += 1;
                println!("Count now {}", self.#ref_count_ident);
                self.#ref_count_ident
            }

            fn release(&mut self) -> u32 {
                self.#ref_count_ident -= 1;
                println!("Count now {}", self.#ref_count_ident);
                let count = self.#ref_count_ident;
                if count == 0 {
                    println!("Count is 0 for {}. Freeing memory...", stringify!(#real_ident));
                    // drop(self)
                    unsafe { Box::from_raw(self as *const _ as *mut #real_ident); }
                }
                count
            }
        }
    )
    // unimplemented!()
}

fn gen_allocate_impl(base_itf_idents: &[Ident], struct_item: &ItemStruct) -> HelperTokenStream {
    let init_ident = &struct_item.ident;
    let real_ident = get_real_ident(&struct_item.ident);

    let mut offset_count: usize = 0;
    let base_inits = base_itf_idents.iter().map(|base| {
        let vtable_var_ident = format_ident!("{}_vtable", base.to_string().to_lowercase());
        let vptr_field_ident = get_vptr_field_ident(&base);

        let out = quote!(
            let #vtable_var_ident = com::vtable!(#real_ident: #base, #offset_count);
            let #vptr_field_ident = Box::into_raw(Box::new(#vtable_var_ident));
        );

        offset_count += 1;
        out
    });
    let base_fields = base_itf_idents.iter().map(|base| {
        let vptr_field_ident = get_vptr_field_ident(base);
        quote!(#vptr_field_ident)
    });
    let ref_count_ident = get_ref_count_ident();
    let inner_init_field_ident = get_inner_init_field_ident();

    quote!(
        impl #real_ident {
            fn allocate(init_struct: #init_ident) -> Box<#real_ident> {
                println!("Allocating new VTable for {}", stringify!(#real_ident));
                #(#base_inits)*
                let out = #real_ident {
                    #(#base_fields,)*
                    #ref_count_ident: 0,
                    #inner_init_field_ident: init_struct
                };
                Box::new(out)
            }
        }
    )
}

fn gen_real_struct(base_itf_idents: &[Ident], struct_item: &ItemStruct) -> HelperTokenStream {
    let init_ident = &struct_item.ident;
    let real_ident = get_real_ident(&struct_item.ident);
    let vis = &struct_item.vis;

    let bases_itf_idents = base_itf_idents.iter().map(|base| {
        let field_ident = get_vptr_field_ident(&base);
        let vptr_ident = get_vptr_ident(&base);
        quote!(#field_ident: #vptr_ident)
    });

    let ref_count_ident = get_ref_count_ident();
    let inner_init_field_ident = get_inner_init_field_ident();

    quote!(
        #[repr(C)]
        #vis struct #real_ident {
            #(#bases_itf_idents,)*
            #ref_count_ident: u32,
            #inner_init_field_ident: #init_ident
        }
    )
}