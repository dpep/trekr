module Inner
  extend ActiveSupport::Concern

  module ClassMethods
    def has_inner
      :inner
    end
  end
end
