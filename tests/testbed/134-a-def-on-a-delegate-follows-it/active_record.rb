module ActiveRecord
  class Relation
    def delete_by(*args); end
    def insert_all(*args); end
  end

  class AssociationRelation < Relation
    def insert_all(*args); end
  end

  module Querying
    delegate :delete_by, :insert_all, to: :all
  end

  class Base
    extend Querying

    def self.all; end
  end
end
