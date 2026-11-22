class ExtendFirst
  extend Finders
  include Lookup
  lookup(1)
end

class ExtendFirstChild < ExtendFirst
  lookup(1)
end

class IncludeFirst
  include Lookup
  extend Finders
  lookup(1)
end

class IncludeFirstChild < IncludeFirst
  lookup(1)
end

class ThroughWrapper
  extend Finders
  include Wrapper
  lookup(1)
end

class WrapperFirst
  include Wrapper
  extend Finders
  lookup(1)
end

class HookFirst
  include Hooked
  extend Finders
  lookup(1)
end

class HookLast
  extend Finders
  include Hooked
  lookup(1)
end

ExtendFirstChild.lookup(1)
IncludeFirstChild.lookup(1)

class LayeredLast
  extend Finders
  include Layered
  lookup(1)
end
