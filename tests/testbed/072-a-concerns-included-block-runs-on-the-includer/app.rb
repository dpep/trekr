module Naming
  def model_name
  end
end

module Api
  extend ActiveSupport::Concern

  included do
    extend Naming

    def self.build
    end

    class << self
      def create
      end
    end
  end
end

class Person
  include Api
end

Person.model_name
Person.build
Person.create
