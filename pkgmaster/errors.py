"""Exception types used across pkgmaster.

The CLI maps these to exit codes and concise messages. Keeping them distinct
matters because "you need a passcode" is a very different answer from "this
file is corrupt" or "this feature is not implemented yet".
"""


class PkgMasterError(Exception):
    """Base class for every expected pkgmaster failure."""


class TruncatedFile(PkgMasterError):
    """The input ended before a structure's declared size."""


class NotAPackage(PkgMasterError):
    """The input has no PS4 CNT or PS5 FIH signature."""


class CorruptImage(PkgMasterError):
    """A structure parsed but its contents are not self-consistent."""


class UnsupportedFeature(PkgMasterError):
    """The image uses a format feature this version does not implement."""


class EncryptedContent(PkgMasterError):
    """Content is encrypted and no usable key was supplied."""


class KeyRequired(EncryptedContent):
    """A passcode or EKPFS key is required to continue."""


class WrongKey(EncryptedContent):
    """A key was supplied, but the resulting image is not readable."""


class ConversionError(PkgMasterError):
    """A content file could not be converted to the requested format."""
