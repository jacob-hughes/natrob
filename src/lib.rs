#![recursion_limit = "512"]

extern crate proc_macro;

use crate::proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, AttributeArgs, ItemTrait, NestedMeta};

#[proc_macro_attribute]
pub fn narrowable(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as AttributeArgs);
    let input = parse_macro_input!(input as ItemTrait);
    if args.len() != 1 {
        panic!("Need precisely one argument to 'narrowable'");
    }
    let struct_id = match &args[0] {
        NestedMeta::Meta(m) => m.name(),
        NestedMeta::Literal(_) => panic!("Literals not valid attributes to 'narrowable'"),
    };
    let trait_id = &input.ident;
    let expanded = quote! {
        /// A narrow pointer to #trait_id.
        #[repr(C)]
        struct #struct_id {
            // A pointer to an object; immediately preceding that object is a usized pointer to the
            // object's vtable. In other words, on a 64 bit machine the layout is (in bytes):
            //   -8..-1: vtable
            //   0..: object
            // Note that:
            //   1) Depending on the alignment of `object`, the allocated block of memory might
            //      start *before* -8 bytes. To calculate the beginning of the block of memory you
            //      need to know the alignment of both the vtable pointer and `object` (see
            //      `Drop::drop` below).
            //   2) If `object` is zero-sized the pointer might be to the very end of the block, so
            //      you mustn't blindly load bytes from this pointer.
            // The reason for this complex dance is that we're trying to optimise the common case
            // of converting this thin pointer into a fat pointer. However, we can only know
            // `object`'s alignment by looking it up in the vtable: if the user doesn't then call
            // anything in the vtable, we've loaded the vtable's cache line for no good reason.
            // Using the layout above, we can avoid doing this load entirely except in the less
            // common case of dropping the pointer.
            objptr: *mut u8
        }

        impl #struct_id {
            /// Create a new narrow pointer to #trait_id.
            pub fn new<U>(v: U) -> Self
            where
                *const U: ::std::ops::CoerceUnsized<*const (dyn #trait_id + 'static)>,
                U: #trait_id + 'static
            {
                let (layout, uoff) = ::std::alloc::Layout::new::<usize>().extend(
                    ::std::alloc::Layout::new::<U>()).unwrap();
                // In order for our storage scheme to work, it's necessary that `uoff -
                // sizeof::<usize>()` gives a valid alignment for a `usize`. There are only two
                // cases we need to consider here:
                //   1) `object`'s alignment is smaller than or equal to `usize`. If so, no padding
                //      will be added, at which point by definition `uoff - sizeof::<usize>()` will
                //      be exactly equivalent to the start point of the layout.
                //   2) `object`'s alignment is bigger than `usize`. Since alignment must be a
                //      power of two, that means that we must by definition be adding at least one
                //      exact multiple of `usize` bytes of padding.
                // The assert below is thus paranoia writ large: it could only trigger if `Layout`
                // started adding amounts of padding that directly contradict the documentation.
                debug_assert_eq!(uoff % ::std::mem::align_of::<usize>(), 0);

                let objptr = unsafe {
                    let baseptr = ::std::alloc::alloc(layout);
                    let objptr = baseptr.add(uoff);
                    let vtableptr = objptr.sub(::std::mem::size_of::<usize>());
                    let t: &dyn #trait_id = &v;
                    let vtable = ::std::mem::transmute::<*const dyn #trait_id, (usize, usize)>(t).1;
                    ::std::ptr::write(vtableptr as *mut usize, vtable);
                    if ::std::mem::size_of::<U>() != 0 {
                        objptr.copy_from_nonoverlapping(&v as *const U as *const u8,
                            ::std::mem::size_of::<U>());
                    }
                    objptr
                };
                ::std::mem::forget(v);

                #struct_id {
                    objptr
                }
            }

            /// Try casting this narrow trait object to a concrete struct type `U`, returning
            /// `Some(...)` if this narrow trait object has stored an object of type `U` or `None`
            /// otherwise.
            pub fn downcast<U: #trait_id>(&self) -> Option<&U> {
                let t_vtable = {
                    let t: *const dyn #trait_id = ::std::ptr::null() as *const U;
                    unsafe { ::std::mem::transmute::<*const dyn #trait_id, (usize, usize)>(t) }.1
                };

                let vtable = unsafe {
                    let vtableptr = self.objptr.sub(::std::mem::size_of::<usize>());
                    ::std::ptr::read(vtableptr as *mut usize)
                };

                if t_vtable == vtable {
                    Some(unsafe { &*(self.objptr as *const U) })
                } else {
                    None
                }
            }
        }

        impl ::std::ops::Deref for #struct_id {
            type Target = dyn #trait_id;

            fn deref(&self) -> &(dyn #trait_id + 'static) {
                unsafe {
                    let vtableptr = self.objptr.sub(::std::mem::size_of::<usize>());
                    let vtable = ::std::ptr::read(vtableptr as *mut usize);
                    ::std::mem::transmute::<(*const _, usize), &dyn #trait_id>(
                        (self.objptr, vtable))
                }
            }
        }

        impl ::std::ops::DerefMut for #struct_id {
            fn deref_mut(&mut self) -> &mut (dyn #trait_id + 'static) {
                unsafe {
                    let vtableptr = self.objptr.sub(::std::mem::size_of::<usize>());
                    let vtable = ::std::ptr::read(vtableptr as *mut usize);
                    ::std::mem::transmute::<(*const _, usize), &mut dyn #trait_id>(
                        (self.objptr, vtable))
                }
            }
        }

        impl ::std::ops::Drop for #struct_id {
            fn drop(&mut self) {
                let fatptr = unsafe {
                    let vtableptr = self.objptr.sub(::std::mem::size_of::<usize>());
                    let vtable = ::std::ptr::read(vtableptr as *mut usize);
                    ::std::mem::transmute::<(*const _, usize), &mut dyn #trait_id>(
                        (self.objptr, vtable))
                };

                // Call `drop` on the trait object before deallocating memory.
                unsafe { ::std::ptr::drop_in_place(fatptr as *mut dyn #trait_id) };

                let align = ::std::mem::align_of_val(fatptr);
                let size = ::std::mem::size_of_val(fatptr);
                unsafe {
                    let (layout, uoff) = ::std::alloc::Layout::new::<usize>().extend(
                        ::std::alloc::Layout::from_size_align_unchecked(size, align)).unwrap();
                    let baseptr = self.objptr.sub(uoff);
                    ::std::alloc::dealloc(baseptr, layout);
                }
            }
        }

        #input
    };

    TokenStream::from(expanded)
}

#[cfg(feature = "rustgc")]
#[proc_macro_attribute]
pub fn narrowable_rustgc(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as AttributeArgs);
    let input = parse_macro_input!(input as ItemTrait);
    if args.len() != 1 {
        panic!("Need precisely one argument to 'narrowable'");
    }
    let struct_short_id = match &args[0] {
        NestedMeta::Meta(m) => m.name(),
        NestedMeta::Literal(_) => panic!("Literals not valid attributes to 'narrowable'"),
    };
    let struct_long_id = syn::Ident::new(
        &format!("__{}_NatRobInternalLong", struct_short_id.to_string()),
        struct_short_id.span(),
    );
    let struct_union_id = syn::Ident::new(
        &format!("__{}_NatRobInternalUnion", struct_short_id.to_string()),
        struct_short_id.span(),
    );
    let trait_id = &input.ident;
    let expanded = quote! {
        // We use this union to type pun between:
        //   short: struct T { vtable } // #struct_short_id
        //   long: struct U { vtable, obj } // #struct_long_id
        // The union and the two structs are all repr(C) to guarantee layout (C99 guarantees that
        // if a union's constituent structs share the same values then one can safely pun between
        // them). We also rely on Rust's guarantee that ManuallyDrop does not affect layout.
        //
        // Note that we do not impose a `#trait_id` constraint on `U` because in
        // `#struct_short_id::deref` we need to reference a concrete type `T` and we can't magic
        // one out of thin air that `:#trait_id`. Instead we create a `struct_union_id<u8>`. This
        // means that we can only create instances of this union that have an alignment less than
        // or equal to that of a `u8`. This is enforced with an `assert` in #struct_short_id::new().
        #[repr(C)]
        union #struct_union_id<U> {
            short: ::std::mem::ManuallyDrop<#struct_short_id>,
            long: ::std::mem::ManuallyDrop<#struct_long_id<U>>,
        }

        unsafe impl<U: Send> Send for #struct_union_id<U> {}
        unsafe impl<U: Sync> Sync for #struct_union_id<U> {}

        unsafe impl<U: FinalizerSafe> FinalizerSafe for #struct_union_id<U> {}
        unsafe impl<U> ::std::gc::FinalizerOptional for #struct_union_id<U> {}

        impl<U> ::std::ops::Drop for #struct_union_id<U> {
            fn drop(&mut self) {
                unsafe {
                    ::std::mem::ManuallyDrop::drop(&mut self.long);
                }
            }
        }

        /// A narrow pointer to #trait_id.
        #[repr(C)]
        pub struct #struct_short_id {
            vtable: *const u8
        }

        impl #struct_short_id {
            /// Create a new narrow pointer to `U: #trait_id`.
            pub fn new<U>(obj: U) -> ::std::gc::Gc<Self>
            where
                *const U: ::std::ops::CoerceUnsized<*const (dyn #trait_id + 'static)>,
                U: #trait_id + 'static
            {
                assert_eq!(::std::mem::align_of::<#struct_long_id<U>>(),
                  ::std::mem::align_of::<#struct_long_id<u8>>());
                let vtable = unsafe { ::std::mem::transmute::
                    <*const dyn #trait_id, (*const u8, *const u8)>(&obj) }
                    .1;
                let gc = ::std::gc::Gc::new(#struct_union_id {
                    long: ::std::mem::ManuallyDrop::new(#struct_long_id { vtable, obj })
                });
                unsafe {
                    ::std::gc::Gc::from_raw(&*gc.short as *const Self)
                }
            }

            pub fn as_gc(&self) -> ::std::gc::Gc<dyn #trait_id> {
                use ::std::ops::Deref;
                Gc::from_raw(self.deref() as *const _)
            }

            /// Convert a downcasted narrow trait object back into a normal narrow trait object.
            /// This will lead to undefined behaviour if `o` was not originally a narrow trait
            /// object.
            pub unsafe fn recover_gc<T: #trait_id>(o: Gc<T>) -> ::std::gc::Gc<#struct_short_id> {
                unsafe {
                    let objptr = Gc::into_raw(o);
                    let baseptr = (objptr as *const usize).sub(1);
                    Gc::from_raw(baseptr as *const u8 as *const #struct_short_id)
                }
            }

            /// Try casting this narrow trait object to a concrete struct type
            /// `U`, returning `Some(...)` if this narrow trait object has
            /// stored an object of type `U` or `None` otherwise.
            pub fn downcast<U: #trait_id>(&self) -> Option<Gc<U>> {
                let t_vtable = {
                    let t: *const dyn #trait_id = ::std::ptr::null() as *const U;
                    unsafe { ::std::mem::transmute::<*const dyn #trait_id, (usize, usize)>(t) }.1
                };

                let vtable = unsafe {
                    ::std::ptr::read(self as *const _ as *const usize)
                };

                if t_vtable == vtable {
                    let objptr = unsafe { (self as *const _ as *const usize).add(1) };
                    Some(unsafe { Gc::from_raw(objptr as *const U) })
                } else {
                    None
                }
            }
        }

        impl ::std::ops::Deref for #struct_short_id {
            type Target = dyn #trait_id;

            fn deref(&self) -> &(dyn #trait_id + 'static) {
                unsafe {
                    let u = &*(self as *const #struct_short_id as *const #struct_union_id<u8>);
                    ::std::mem::transmute::<(*const _, *const _), &dyn #trait_id>(
                      (&u.long.obj as *const _, u.long.vtable))
                }
            }
        }

        #[repr(C)]
        struct #struct_long_id<U> {
            vtable: *const u8,
            obj: U
        }

        #input
    };
    TokenStream::from(expanded)
}
