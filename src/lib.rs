#![recursion_limit = "256"]

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
        NestedMeta::Literal(_) => panic!("Literals not valid attributes to 'narrowable'")
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

#[cfg(feature = "rboehm")]
#[proc_macro_attribute]
pub fn narrowable_rboehm(args: TokenStream, input: TokenStream) -> TokenStream {
    let args = parse_macro_input!(args as AttributeArgs);
    let input = parse_macro_input!(input as ItemTrait);
    if args.len() != 1 {
        panic!("Need precisely one argument to 'narrowable'");
    }
    let struct_id = match &args[0] {
        NestedMeta::Meta(m) => m.name(),
        NestedMeta::Literal(_) => panic!("Literals not valid attributes to 'narrowable'")
    };
    let trait_id = &input.ident;
    let expanded = quote! {
        /// A narrow pointer to #trait_id.
        pub struct #struct_id {
            // This struct points to a vtable pointer followed by an object. In other words, on a
            // 64 bit machine the layout is (in bytes):
            //   0..7: vtable
            //   8..: object
            // This is an inflexible layout, since we can only support structs whose alignment is
            // the same or less than a usize's.
            vtable: *mut u8
        }

        impl #struct_id {
            /// Create a new narrow pointer to #trait_id.
            pub fn new<U>(v: U) -> ::rboehm::Gc<Self>
            where
                *const U: ::std::ops::CoerceUnsized<*const (dyn #trait_id + 'static)>,
                U: #trait_id + 'static
            {
                let (layout, uoff) = ::std::alloc::Layout::new::<usize>().extend(
                    ::std::alloc::Layout::new::<U>()).unwrap();
                // Check that we've not been given an object whose alignment
                // exceeds that of a usize.
                debug_assert_eq!(uoff, ::std::mem::size_of::<usize>());

                let gc = ::rboehm::Gc::<#struct_id>::new_from_layout(layout);
                let baseptr = ::rboehm::Gc::into_raw(gc);
                unsafe {
                    let objptr = (baseptr as *mut u8).add(uoff);
                    let t: &dyn #trait_id = &v;
                    let vtable = ::std::mem::transmute::<*const dyn #trait_id, (usize, usize)>(t).1;
                    ::std::ptr::write(baseptr as *mut usize, vtable);

                    if ::std::mem::size_of::<U>() != 0 {
                        objptr.copy_from_nonoverlapping(&v as *const U as *const u8,
                            ::std::mem::size_of::<U>());
                    }

                }
                ::std::mem::forget(v);
                unsafe { gc.assume_init() }
            }

            /// Create a narrow pointer to #trait_id. `layout` must be at least big enough for an
            /// object of type `U` (but may optionally be bigger) and must have at least the same
            /// alignment that `U requires (but may optionally have a bigger alignment). `init`
            /// will be called with a pointer to uninitialised memory into which a fully
            /// initialised object of type `U` *must* be written. After `init` completes, the
            /// object will be considered fully initialised: failure to fully initialise it causes
            /// undefined behaviour. Note that if additional memory was requested beyond that
            /// needed to store `U` then that extra memory does not have to be initialised after
            /// `init` completes.
            pub unsafe fn new_from_layout<U: #trait_id, F>(layout: ::std::alloc::Layout,
                init: F) -> ::rboehm::Gc<Self>
                where F: FnOnce(*mut U)
            {
                let (layout, uoff) = ::std::alloc::Layout::new::<usize>().extend(layout).unwrap();
                // Check that we've not been given an object whose alignment
                // exceeds that of a usize.
                debug_assert_eq!(uoff, ::std::mem::size_of::<usize>());

                let gc = ::rboehm::Gc::<Self>::new_from_layout(layout);
                let baseptr = ::rboehm::Gc::into_raw(gc);
                unsafe {
                    let objptr = (baseptr as *mut u8).add(uoff);
                    let t: *const dyn #trait_id = objptr as *const U;
                    let vtable = ::std::mem::transmute::<*const dyn #trait_id, (usize, usize)>(t).1;
                    ::std::ptr::write(baseptr as *mut usize, vtable);
                    init(objptr as *mut U);
                    gc.assume_init()
                }
            }

            pub fn as_gc(&self) -> ::rboehm::Gc<dyn #trait_id> {
                use ::std::ops::Deref;
                Gc::from_raw(self.deref() as *const _)
            }

            /// Convert a downcasted narrow trait object back into a normal narrow trait object.
            /// This will lead to undefined behaviour if `o` was not originally a narrow trait
            /// object.
            pub unsafe fn recover_gc<T: #trait_id>(o: Gc<T>) -> ::rboehm::Gc<#struct_id> {
                unsafe {
                    let objptr = Gc::into_raw(o);
                    let baseptr = (objptr as *const usize).sub(1);
                    Gc::from_raw(baseptr as *const u8 as *const #struct_id)
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

        impl ::std::ops::Deref for #struct_id {
            type Target = dyn #trait_id;

            fn deref(&self) -> &(dyn #trait_id + 'static) {
                unsafe {
                    let vtable = ::std::ptr::read(self as *const _ as *const usize as *mut usize);
                    let objptr = (self as *const _ as *const usize).add(1);
                    ::std::mem::transmute::<(*const _, usize), &dyn #trait_id>(
                        (objptr, vtable))
                }
            }
        }

        impl ::std::ops::Drop for #struct_id {
            fn drop(&mut self) {
                let fatptr = unsafe {
                    let vtable = ::std::ptr::read(self as *const _ as *const usize as *mut usize);
                    let objptr = (self as *const _ as *const usize).add(1);
                    ::std::mem::transmute::<(*const _, usize), &mut dyn #trait_id>(
                        (objptr, vtable))
                };

                // Call `drop` on the trait object before deallocating memory.
                unsafe { ::std::ptr::drop_in_place(fatptr as *mut dyn #trait_id) };
            }
        }

        #input
    };
    TokenStream::from(expanded)
}
