module Aliased
  C = ActiveSupport::Concern
  extend C
  module ClassMethods
    def aliased_fields; 1; end
  end
end
