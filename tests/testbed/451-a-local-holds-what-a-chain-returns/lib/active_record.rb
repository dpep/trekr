module ActiveRecord
  module QueryMethods
    def where(*args)
      spawn
    end

    def limit(n)
      spawn
    end

    def order(*args)
      spawn
    end
  end

  module Calculations
    def pluck(*names)
    end
  end

  class Relation
    include QueryMethods
    include Calculations
  end

  module Associations
    class CollectionProxy < Relation
      def find(*args)
      end
    end
  end

  module Querying
    delegate :where, :order, :limit, to: :all
  end

  class Base
    extend Querying
  end
end

class OptionParser
  def order(*args)
  end

  def pluck(*names)
  end

  def find(*args)
  end
end

class Pager
  def limit(n)
  end
end
