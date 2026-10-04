module Store
  module Adapters
    module Pg
      class SchemaDumper < SchemaDumper
        def tables
          :pg
        end
      end
    end
  end
end
