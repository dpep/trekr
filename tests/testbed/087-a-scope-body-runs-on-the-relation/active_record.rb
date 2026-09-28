module ActiveRecord
  module QueryMethods
    def where(*conditions); end
  end

  class Relation
    include QueryMethods
  end

  module Querying
    def where(*conditions); end
  end

  class Base
    extend Querying
  end
end
