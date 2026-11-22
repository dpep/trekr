module Finders
  def lookup(id)
    :finders
  end
end

module Lookup
  extend ActiveSupport::Concern

  module ClassMethods
    def lookup(id)
      :lookup
    end
  end
end

module Wrapper
  extend ActiveSupport::Concern
  include Lookup
end

module Hooked
  def self.included(base)
    base.extend(Helpers)
  end

  module Helpers
    def lookup(id)
      :hooked
    end
  end
end

module Layered
  extend ActiveSupport::Concern
  include Lookup

  module ClassMethods
    def lookup(id)
      :layered
    end
  end
end
