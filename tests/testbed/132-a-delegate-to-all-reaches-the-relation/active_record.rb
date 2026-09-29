module ActiveRecord
  class Relation
    def delete_by(*args); end
    def where(*args); end
  end

  module Querying
    delegate :delete_by, :where, to: :all
  end

  class Base
    extend Querying

    def self.all; end
  end
end
